//! Spike: keep a workspace loaded in rust-analyzer's database and compile
//! functions to native code that links against rustc-built artifacts.

mod symbols;

use ide_db::base_db::SourceDatabase as _;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, bail};
use hir::{AsAssocItem, AssocItemContainer, Crate, Module, ModuleDef, db::HirDatabase};
use hir_def::{
    AssocItemId, DefWithBodyId, FunctionId, ItemContainerId, Lookup, ModuleDefId, StaticId,
    expr_store::Body, item_scope::ItemScope,
};
use hir_ty::mir::codegen::{
    CodegenError, CodegenUnit, Rejections, Roots, SymbolResolver, exported_symbol,
};
use ide_db::{ChangeWithProcMacros, RootDatabase};
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use project_model::{CargoConfig, CargoFeatures, RustLibSource};
use rustc_hash::{FxHashMap, FxHashSet};
use syntax::{
    AstNode, SyntaxNode,
    ast::{self, HasName},
    match_ast,
};
use vfs::{AbsPathBuf, Vfs, VfsPath};

use crate::symbols::SymbolTable;

struct Args {
    project: PathBuf,
    krate: String,
    /// More crates to compile from the same loaded workspace, into the same object. When `out`
    /// is a directory, each crate is a program of its own instead: `out` and `report` get a
    /// file per crate, and a crate that fails gets a `.error` file instead of ending the run.
    batch: Vec<String>,
    out: PathBuf,
    symbol_files: Vec<PathBuf>,
    watch: bool,
    link_cmd: Option<String>,
    /// Where to write the object that amends `out` after an edit, for the link to take ahead of
    /// `out`. Without it every edit writes `out` again.
    patch: Option<PathBuf>,
    /// Tab-separated per-function outcome, for the harness.
    report: Option<PathBuf>,
    /// File of function paths to leave to rustc, for bisecting miscompiles.
    deny: Option<PathBuf>,
    /// Directory that gets one object per compiled function, for testing each in isolation.
    isolate: Option<PathBuf>,
    /// Build the workspace's crates as `cargo test` does, with `cfg(test)`.
    cfg_test: bool,
    /// Cargo features to build the workspace's crates with, on top of the default ones.
    features: Vec<String>,
}

impl Args {
    /// The directory of the project, which cargo runs rustc in.
    fn root(&self) -> anyhow::Result<PathBuf> {
        let mut root = std::fs::canonicalize(&self.project)?;
        if root.is_file() {
            root.pop();
        }
        Ok(root)
    }

    fn parse() -> anyhow::Result<Args> {
        let mut args = std::env::args().skip(1);
        let mut project = None;
        let mut krate = None;
        let mut batch = Vec::new();
        let mut out = None;
        let mut symbol_files = Vec::new();
        let mut watch = false;
        let mut link_cmd = None;
        let mut patch = None;
        let mut report = None;
        let mut deny = None;
        let mut isolate = None;
        let mut cfg_test = false;
        let mut features = Vec::new();
        while let Some(arg) = args.next() {
            let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--crate" if krate.is_some() => batch.push(value()?),
                "--crate" => krate = Some(value()?),
                "--out" => out = Some(PathBuf::from(value()?)),
                "--symbols" => symbol_files.push(PathBuf::from(value()?)),
                "--link-cmd" => link_cmd = Some(value()?),
                "--patch" => patch = Some(PathBuf::from(value()?)),
                "--report" => report = Some(PathBuf::from(value()?)),
                "--deny" => deny = Some(PathBuf::from(value()?)),
                "--isolate" => isolate = Some(PathBuf::from(value()?)),
                "--watch" => watch = true,
                "--cfg-test" => cfg_test = true,
                "--features" => features.extend(value()?.split(',').map(str::to_owned)),
                _ if project.is_none() => project = Some(PathBuf::from(arg)),
                _ => bail!("unexpected argument {arg}"),
            }
        }
        Ok(Args {
            project: project.context("missing project path")?,
            krate: krate.context("missing --crate")?,
            batch,
            out: out.context("missing --out")?,
            symbol_files,
            watch,
            link_cmd,
            patch,
            report,
            deny,
            isolate,
            cfg_test,
            features,
        })
    }
}

struct Resolver<'a> {
    db: &'a RootDatabase,
    table: &'a SymbolTable,
    /// What panic locations in the workspace are relative to.
    root: PathBuf,
    rustc_commit: std::cell::OnceCell<Option<String>>,
}

impl Resolver<'_> {
    /// The function's path as `rustc-demangle` prints its v0 symbol.
    fn path(&self, func: FunctionId) -> Option<String> {
        let ModuleDef::Function(func) = ModuleDef::from(ModuleDefId::FunctionId(func)) else {
            return None;
        };
        let name = func.name(self.db);
        let Some(assoc) = func.as_assoc_item(self.db) else {
            return self.item_path(func.module(self.db), name.as_str());
        };
        let AssocItemContainer::Impl(imp) = assoc.container(self.db) else { return None };
        let self_ty = self.type_path(&imp.self_ty(self.db))?;
        match imp.trait_(self.db) {
            Some(trait_) => {
                let mut path =
                    self.item_path(trait_.module(self.db), trait_.name(self.db).as_str())?;
                // `impl PartialEq<str> for String` and `impl PartialEq<&str> for String` are
                // told apart by the trait's arguments alone.
                let trait_ref = imp.trait_ref(self.db)?;
                let params = hir::GenericDef::from(trait_).type_or_const_params(self.db);
                let mut args = Vec::new();
                for (idx, param) in params.iter().enumerate().skip(1) {
                    let arg = trait_ref.get_type_argument(idx)?;
                    let defaulted = param
                        .as_type_param(self.db)
                        .is_some_and(|it| it.default(self.db).is_some());
                    // FIXME: the only default understood is `= Self`.
                    let is_self = hir::attach_db(self.db, || arg == trait_ref.self_ty());
                    if defaulted && idx + 1 == params.len() && is_self {
                        break;
                    }
                    args.push(self.type_path(&arg)?);
                }
                if !args.is_empty() {
                    path = format!("{path}<{}>", args.join(", "));
                }
                let trait_ = path;
                Some(format!("<{self_ty} as {trait_}>::{}", name.as_str()))
            }
            None => Some(format!("<{self_ty}>::{}", name.as_str())),
        }
    }

    /// The file the crate's source starts at.
    fn crate_root(&self, krate: Crate) -> Option<PathBuf> {
        let file = krate.root_file(self.db);
        let source_root = self.db.file_source_root(file).source_root_id(self.db);
        let source_root = self.db.source_root(source_root).source_root(self.db);
        Some(PathBuf::from(source_root.path_for_file(&file)?.as_path()?.to_string()))
    }

    fn item_path(&self, module: Module, name: &str) -> Option<String> {
        let mut path = self.scope_path(module)?;
        path.push_str("::");
        path.push_str(name);
        Some(path)
    }

    /// The function written at `func`, which holds the block that `scope` is the module of.
    ///
    /// `Semantics::to_def` answers the same, but starts from every crate the file is part of,
    /// which for a package with tests and examples analyses all its dev-dependencies.
    fn function_at(
        &self,
        sema: &hir::Semantics<'_, RootDatabase>,
        scope: Module,
        func: &ast::Fn,
    ) -> Option<hir::Function> {
        use hir::HasSource;
        let db = self.db;
        let file = sema.hir_file_for(func.syntax());
        let range = func.syntax().text_range();
        let is_it = |it: &hir::Function| {
            it.source(db).is_some_and(|source| {
                source.file_id == file && source.value.syntax().text_range() == range
            })
        };
        let among = |items: Vec<hir::AssocItem>| {
            items.into_iter().filter_map(hir::AssocItem::as_function).find(is_it)
        };
        let mut module = Some(scope);
        while let Some(it) = module {
            let found = it
                .declarations(db)
                .into_iter()
                .find_map(|def| match def {
                    ModuleDef::Function(func) => Some(func).filter(is_it),
                    ModuleDef::Trait(trait_) => among(trait_.items(db)),
                    _ => None,
                })
                .or_else(|| it.impl_defs(db).into_iter().find_map(|imp| among(imp.items(db))));
            if found.is_some() || it.nearest_non_block_module(db) == it {
                return found;
            }
            module = it.parent(db);
        }
        None
    }

    /// The path of what holds a module's items: the module, or for the items of a block the
    /// function, closure, constant or static the block is in, which only the syntax knows.
    fn scope_path(&self, module: Module) -> Option<String> {
        if module.nearest_non_block_module(self.db) == module {
            let krate = module.krate(self.db);
            let krate_name = krate.display_name(self.db)?;
            let mut path = self
                .table
                .crate_path(krate_name.crate_name().as_str(), &self.crate_root(krate)?)?;
            for segment in module.path_segments(self.db) {
                path.push_str("::");
                path.push_str(segment.as_str());
            }
            return Some(path);
        }
        let sema = hir::Semantics::new(self.db);
        let source = module.definition_source(self.db);
        let hir::ModuleSource::BlockExpr(block) = source.value else { return None };
        let range = block.syntax().text_range();
        let block = sema
            .parse_or_expand(source.file_id)
            .covering_element(range)
            .ancestors()
            .find(|it| it.text_range() == range && ast::BlockExpr::can_cast(it.kind()))?;
        let mut inner = Vec::new();
        let mut owner = None;
        for node in sema.ancestors_with_macros(block) {
            if let Some(func) = ast::Fn::cast(node.clone()) {
                let func = self.function_at(&sema, module, &func)?;
                owner = Some(self.path(func.try_into().ok()?)?);
                break;
            }
            match_ast! {
                match node {
                    ast::ClosureExpr(it) => inner.push(format!("{{closure#{}}}", closure_number(&it))),
                    ast::Const(it) => inner.push(it.name().map_or("_".to_owned(), |it| it.text().to_string())),
                    ast::Static(it) => inner.push(it.name()?.text().to_string()),
                    // Anonymous constants have numbers we don't know.
                    ast::BlockExpr(it) => if it.const_token().is_some() { return None },
                    ast::ConstArg(_) => return None,
                    _ => {},
                }
            }
        }
        let mut path = match owner {
            Some(owner) => owner,
            None => {
                let outer = inner.pop()?;
                self.item_path(module.nearest_non_block_module(self.db), &outer)?
            }
        };
        for segment in inner.iter().rev() {
            path.push_str("::");
            path.push_str(segment);
        }
        Some(path)
    }

    /// A type the way rustc writes it in a symbol. Generic ADTs have no symbol we can name yet.
    fn type_path(&self, ty: &hir::Type<'_>) -> Option<String> {
        if let Some((inner, mutability)) = ty.as_reference() {
            let inner = self.type_path(&inner)?;
            return Some(match mutability {
                hir::Mutability::Shared => format!("&{inner}"),
                hir::Mutability::Mut => format!("&mut {inner}"),
            });
        }
        if let Some(elem) = ty.as_slice() {
            return Some(format!("[{}]", self.type_path(&elem)?));
        }
        if let Some(builtin) = ty.as_builtin() {
            return Some(builtin.name().as_str().to_owned());
        }
        let adt = ty.as_adt()?;
        // rustc leaves a defaulted const argument out of the name, so `Bump` may be `Bump<1>`.
        if !hir::GenericDef::from(adt).type_or_const_params(self.db).is_empty() {
            return None;
        }
        self.item_path(adt.module(self.db), adt.name(self.db).as_str())
    }
}

/// The commit of the toolchain installed at `toolchain`, which rustc puts in the names of the
/// standard library's files.
fn rustc_commit(toolchain: &str) -> Option<String> {
    let manifest =
        std::fs::read_to_string(format!("{toolchain}lib/rustlib/multirust-channel-manifest.toml"))
            .ok()?;
    let (_, rust) = manifest.split_once("[pkg.rust]")?;
    let (_, commit) = rust.split_once("git_commit_hash = \"")?;
    Some(commit.split_once('"')?.0.to_owned())
}

impl SymbolResolver for Resolver<'_> {
    fn source_path(&self, file: vfs::FileId) -> Option<String> {
        let source_root = self.db.file_source_root(file).source_root_id(self.db);
        let source_root = self.db.source_root(source_root).source_root(self.db);
        let path = source_root.path_for_file(&file)?.as_path()?.to_string();
        if let Some((toolchain, file)) = path.split_once("lib/rustlib/src/rust/")
            && let Some(commit) = self.rustc_commit.get_or_init(|| rustc_commit(toolchain))
        {
            return Some(format!("/rustc/{commit}/{file}"));
        }
        let relative = Path::new(&path).strip_prefix(&self.root).ok();
        Some(relative.map_or(path.clone(), |it| it.to_string_lossy().into_owned()))
    }

    fn function(&self, func: FunctionId) -> Option<String> {
        if let Some(symbol) = exported_symbol(self.db, func) {
            return Some(symbol);
        }
        self.table.get(&self.path(func)?).map(str::to_owned)
    }

    fn stable_crate_id(&self, krate: ide_db::base_db::Crate) -> Option<u64> {
        let krate = Crate::from(krate);
        let name = krate.display_name(self.db)?;
        self.table.stable_crate_id(name.crate_name().as_str(), &self.crate_root(krate)?)
    }

    fn static_(&self, id: StaticId) -> Option<String> {
        let ModuleDef::Static(it) = ModuleDef::from(ModuleDefId::StaticId(id)) else {
            return None;
        };
        let module = it.module(self.db);
        let name = it.name(self.db);
        if module.nearest_non_block_module(self.db) == module {
            let path = self.item_path(module, name.as_str())?;
            return self.table.get(&path).map(str::to_owned);
        }
        // A static in a body is named after the items around it, which only the syntax knows.
        let sema = hir::Semantics::new(self.db);
        let source = sema.source(it)?;
        let mut names = vec![name.as_str().to_owned()];
        let mut closures = Vec::new();
        let mut owner = None;
        for node in sema.ancestors_with_macros(source.value.syntax().clone()).skip(1) {
            if let Some(closure) = ast::ClosureExpr::cast(node.clone()) {
                closures.push(closure_number(&closure));
                continue;
            }
            if let Some(func) = ast::Fn::cast(node.clone()) {
                let func = self.function_at(&sema, module, &func)?;
                owner = Some(self.path(func.try_into().ok()?)?);
                break;
            }
            let name = match_ast! {
                match node {
                    ast::Const(it) => it.name(),
                    ast::Static(it) => it.name(),
                    _ => None,
                }
            };
            names.extend(name.map(|it| it.text().to_string()));
        }
        let owner = match owner {
            Some(owner) => owner,
            None => {
                let outer = names.pop()?;
                self.item_path(module.nearest_non_block_module(self.db), &outer)?
            }
        };
        names.push(owner);
        names.reverse();
        closures.reverse();
        let path = names.join("::");
        // Only a path through a closure or an anonymous constant has segments left out of it.
        let symbol = self.table.get_nested(&path, &closures).or_else(|| self.table.get(&path));
        symbol.map(str::to_owned)
    }

    fn path(&self, path: &str) -> Option<String> {
        self.table.get(path).map(str::to_owned)
    }

    fn item(&self, item: ModuleDefId) -> Option<String> {
        let item = ModuleDef::from(item);
        self.item_path(item.module(self.db)?, item.name(self.db)?.as_str())
    }
}

/// The `n` of the `{closure#n}` rustc names a closure: its place among the closures of the
/// definition it is written in, a closure's own closures being counted apart.
fn closure_number(closure: &ast::ClosureExpr) -> u32 {
    let is_definition = |node: &SyntaxNode| {
        ast::Item::can_cast(node.kind())
            || ast::AssocItem::can_cast(node.kind())
            || ast::ClosureExpr::can_cast(node.kind())
            || ast::ConstArg::can_cast(node.kind())
            || ast::BlockExpr::cast(node.clone()).is_some_and(|it| it.const_token().is_some())
    };
    let owner_of = |node: &SyntaxNode| node.ancestors().skip(1).find(is_definition);
    let owner = owner_of(closure.syntax());
    let root = owner.clone().unwrap_or_else(|| closure.syntax().ancestors().last().unwrap());
    let earlier = root
        .descendants()
        .take_while(|node| node != closure.syntax())
        .filter(|node| ast::ClosureExpr::can_cast(node.kind()) && owner_of(node) == owner);
    earlier.count() as u32
}

fn crate_named(db: &RootDatabase, krate: &str) -> anyhow::Result<Crate> {
    Crate::all(db)
        .into_iter()
        .filter(|it| it.display_name(db).is_some_and(|name| name.crate_name().as_str() == krate))
        // A dependency may be another version of the crate being built.
        .min_by_key(|it| !it.origin(db).is_local())
        .with_context(|| format!("crate {krate} not found in workspace"))
}

/// Resolves the names and collects the impls of the crates being built and of everything they
/// depend on, a crate per thread. Every build needs them, and one thread would do them in turn.
fn resolve_crates(db: &RootDatabase, args: &Args) -> anyhow::Result<usize> {
    let mut seen = FxHashSet::default();
    let mut crates = Vec::new();
    for name in std::iter::once(&args.krate).chain(&args.batch) {
        let krate: ide_db::base_db::Crate = crate_named(db, name)?.into();
        crates.extend(krate.transitive_deps(db).into_iter().filter(|&dep| seen.insert(dep)));
    }
    // A crate comes before its dependencies in the list, and a thread that starts on one
    // waits for the threads working on what it depends on.
    crates.reverse();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |it| it.get());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let db = db.clone();
            let (crates, next) = (&crates, &next);
            scope.spawn(move || {
                while let Some(&krate) =
                    crates.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    hir::attach_db(&db, || {
                        _ = hir::crate_def_map(&db, krate);
                        _ = hir::TraitImpls::for_crate(&db, krate);
                    });
                }
            });
        }
    });
    Ok(crates.len())
}

fn functions_of(db: &RootDatabase, krate: &str) -> anyhow::Result<Vec<FunctionId>> {
    let krate = crate_named(db, krate)?;
    let mut modules: Vec<Module> = vec![krate.root_module(db)];
    let mut functions = Vec::new();
    // Each scope's functions, and the bodies whose blocks declare more scopes.
    let mut bodies: Vec<DefWithBodyId> = Vec::new();
    let mut scope = |scope: &ItemScope, bodies: &mut Vec<DefWithBodyId>| {
        for def in scope.declarations() {
            match def {
                // A function declared in an `extern` block has no body to compile.
                ModuleDefId::FunctionId(id)
                    if !matches!(id.lookup(db).container, ItemContainerId::ExternBlockId(_)) =>
                {
                    functions.push(id);
                    bodies.push(id.into());
                }
                ModuleDefId::ConstId(id) => bodies.push(id.into()),
                ModuleDefId::StaticId(id) => bodies.push(id.into()),
                _ => {}
            }
        }
        bodies.extend(scope.unnamed_consts().map(DefWithBodyId::from));
        for imp in scope.impls() {
            for &(_, item) in imp.impl_items(db).items.iter() {
                match item {
                    AssocItemId::FunctionId(id) => {
                        functions.push(id);
                        bodies.push(id.into());
                    }
                    AssocItemId::ConstId(id) => bodies.push(id.into()),
                    AssocItemId::TypeAliasId(_) => {}
                }
            }
        }
    };
    while let Some(module) = modules.pop() {
        modules.extend(module.children(db));
        let id = hir_def::ModuleId::from(module);
        scope(&id.def_map(db)[id].scope(db), &mut bodies);
    }
    // A body's blocks include those of the blocks in it, but not those of the items in them.
    // An edit to a file has every body in it lowered again, which is most of the work here, so
    // each round of bodies is lowered on all threads before it is read.
    let threads = std::thread::available_parallelism().map_or(1, |it| it.get());
    while !bodies.is_empty() {
        let round = std::mem::take(&mut bodies);
        std::thread::scope(|threads_scope| {
            for part in round.chunks(round.len().div_ceil(threads)) {
                let db = db.clone();
                threads_scope.spawn(move || {
                    hir::attach_db(&db, || {
                        for &body in part {
                            Body::of(&db, body);
                        }
                    })
                });
            }
        });
        for body in round {
            for (_, def_map) in Body::of(db, body).blocks(db) {
                for (_, module) in def_map.modules() {
                    scope(module.scope(db), &mut bodies);
                }
            }
        }
    }
    Ok(functions)
}

struct Build {
    compiled: Vec<String>,
    skipped: Vec<(String, CodegenError)>,
    instances: usize,
    codegen: Duration,
    emit: Duration,
    /// How long finding the roots, checking what is cached and compiling took, in that order.
    steps: [Duration; 3],
    /// Within the checking, the longest a thread took over each kind of cached thing.
    looked: hir_ty::mir::codegen::StaleSteps,
}

thread_local! {
    /// Set on a thread that asks about what an edit may have removed. A query about something
    /// that is gone panics, which is the answer and not worth a report.
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn build(
    db: &RootDatabase,
    table: &SymbolTable,
    args: &Args,
    known: &mut Rejections,
) -> anyhow::Result<Build> {
    let start = Instant::now();
    let root = args.root()?;
    let resolver = Resolver { db, table, root, rustc_commit: Default::default() };
    let hir_db: &dyn HirDatabase = db;
    let mut functions = functions_of(db, &args.krate)?;
    for krate in &args.batch {
        functions.extend(functions_of(db, krate)?);
    }
    // With a program to start from, only what it reaches is compiled, and what a runtime
    // looks up by name. Without one any function may be the one called.
    let is_main = |&func: &FunctionId| hir::Function::from(func).is_main(db);
    let (entries, on_demand): (Vec<_>, Vec<_>) = if functions.iter().any(is_main) {
        functions
            .into_iter()
            .partition(|func| is_main(func) || exported_symbol(db, *func).is_some())
    } else {
        (functions, Vec::new())
    };
    let roots = Roots { entries: &entries, on_demand: &on_demand };
    let setup = start.elapsed();
    // Not dropped from the roots: what they share with the others has to stay with rustc too.
    let mut denied = FxHashSet::default();
    if let Some(deny) = &args.deny {
        let deny = std::fs::read_to_string(deny)?;
        let deny: FxHashSet<&str> = deny.lines().collect();
        denied.extend(
            entries.iter().chain(&on_demand).copied().filter(|&func| {
                resolver.path(func).is_some_and(|path| deny.contains(path.as_str()))
            }),
        );
    }
    let threads = std::thread::available_parallelism().map_or(1, |it| it.get());
    let stale: Option<Vec<_>> = std::thread::scope(|scope| {
        let parts: Vec<_> = (0..threads)
            .map(|part| {
                let (db, known) = (db.clone(), &*known);
                scope.spawn(move || {
                    PROBING.set(true);
                    hir::attach_db(&db, || known.stale(&db, part, threads))
                })
            })
            .collect();
        parts.into_iter().map(|it| it.join().ok().flatten()).collect()
    });
    known.set_stale(stale.unwrap_or_default());
    let looked = known.stale_steps();
    let revalidate = start.elapsed();
    // Salsa attaches the database for the duration of the closure; the solver needs it.
    let unit =
        hir::attach_db(db, || CodegenUnit::compile(hir_db, &resolver, roots, &denied, known))
            .map_err(anyhow::Error::msg)?;
    let name = |func| resolver.path(func).unwrap_or_else(|| "<assoc fn>".to_owned());
    let codegen = start.elapsed();
    if std::env::var_os("RUSTD_STATS").is_some() {
        eprintln!(
            "roots: {setup:.1?}, revalidate: {:.1?}, compile: {:.1?}",
            revalidate - setup,
            codegen - revalidate
        );
    }
    let start = Instant::now();
    if let Some(object) = &unit.object {
        std::fs::write(&args.out, object)?;
    }
    if let (Some(path), Some(patch)) = (&args.patch, &unit.patch) {
        std::fs::write(path, patch)?;
    }
    let build = Build {
        compiled: unit.compiled.iter().map(|&(func, _)| name(func)).collect(),
        skipped: unit.skipped.into_iter().map(|(func, e)| (name(func), e)).collect(),
        instances: unit.instances,
        codegen,
        emit: start.elapsed(),
        steps: [setup, revalidate - setup, codegen - revalidate],
        looked,
    };
    if let Some(path) = &args.report {
        build.write_report(path)?;
    }
    if let Some(dir) = &args.isolate {
        std::fs::create_dir_all(dir)?;
        let mut index = String::new();
        for (i, &(func, _)) in unit.compiled.iter().enumerate() {
            let single = hir::attach_db(db, || {
                let roots = Roots { entries: &[func], on_demand: &[] };
                CodegenUnit::compile(hir_db, &resolver, roots, &Default::default(), known)
            })
            .map_err(anyhow::Error::msg)?;
            std::fs::write(dir.join(format!("{i}.o")), single.object.unwrap_or_default())?;
            index.push_str(&format!("{i}\t{}\n", name(func)));
        }
        std::fs::write(dir.join("index.tsv"), index)?;
    }
    Ok(build)
}

impl Build {
    fn write_report(&self, path: &Path) -> anyhow::Result<()> {
        use std::fmt::Write;
        let mut out = String::new();
        writeln!(
            out,
            "#\tinstances\t{}\tcodegen_us\t{}",
            self.instances,
            self.codegen.as_micros()
        )?;
        for name in &self.compiled {
            writeln!(out, "compiled\t{name}\t")?;
        }
        for (name, error) in &self.skipped {
            let error = error.to_string().replace(['\t', '\n'], " ");
            writeln!(out, "skipped\t{name}\t{error}")?;
        }
        Ok(std::fs::write(path, out)?)
    }
}

fn report(build: &Build) {
    eprintln!(
        "compiled {} of {} fns (+{} instances) in {:.1?} (+{:.1?} emit)",
        build.compiled.len(),
        build.compiled.len() + build.skipped.len(),
        build.instances,
        build.codegen,
        build.emit,
    );
    if build.skipped.len() <= 8 {
        for (name, error) in &build.skipped {
            eprintln!("  left to rustc: {name}: {error}");
        }
        return;
    }
    // Bucket by the innermost reason, without the payload.
    let mut reasons: FxHashMap<String, (usize, &str)> = FxHashMap::default();
    for (name, error) in &build.skipped {
        let error = error.to_string();
        let reason = error.rsplit("`: ").next().unwrap_or(&error);
        let reason = reason.split(['(', '{']).next().unwrap_or(reason).trim();
        let entry = reasons.entry(reason.to_owned()).or_insert((0, name));
        entry.0 += 1;
    }
    let mut reasons: Vec<_> = reasons.into_iter().collect();
    reasons.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    for (reason, (count, example)) in reasons {
        eprintln!("  {count:>5}  {reason}  (e.g. {example})");
    }
}

fn link(args: &Args) -> anyhow::Result<()> {
    let Some(cmd) = &args.link_cmd else { return Ok(()) };
    let start = Instant::now();
    let status = Command::new("sh").arg("-c").arg(cmd).status()?;
    eprintln!("link: {:.1?} ({status})", start.elapsed());
    Ok(())
}

/// A file whose changes the daemon picks up.
enum Watched {
    Source(vfs::FileId),
    /// Named by an `include_bytes!` or `include_str!`, and not a source file.
    Included(AbsPathBuf),
}

struct WatchedFile {
    file: Watched,
    path: PathBuf,
    mtime: SystemTime,
}

impl WatchedFile {
    fn new(file: Watched, path: &Path) -> Option<WatchedFile> {
        // Everything but the sysroot and the registry: path dependencies live outside the project.
        let immutable = path
            .components()
            .any(|c| matches!(c.as_os_str().to_str(), Some("target" | "rustlib" | "registry")));
        if immutable {
            return None;
        }
        let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
        Some(WatchedFile { file, path: path.to_owned(), mtime })
    }

    fn sources(vfs: &Vfs) -> Vec<WatchedFile> {
        vfs.iter()
            .filter_map(|(id, path)| {
                WatchedFile::new(Watched::Source(id), path.as_path()?.as_ref())
            })
            .collect()
    }

    /// Adds the files the last build included that are not watched yet. One is read again once
    /// its timestamp is taken, so a write between the build's read and now is not lost.
    fn watch_included(files: &mut Vec<WatchedFile>, db: &mut RootDatabase) {
        let watched: FxHashSet<&Path> = files.iter().map(|it| &*it.path).collect();
        let new: Vec<AbsPathBuf> = db
            .included_files()
            .into_iter()
            .filter(|it| !watched.contains(AsRef::<Path>::as_ref(it)))
            .collect();
        for path in new {
            if let Some(file) = WatchedFile::new(Watched::Included(path.clone()), path.as_ref()) {
                files.push(file);
                db.reload_included_file(&path);
            }
        }
    }
}

/// Compiles every crate asked for, each as it would be alone.
/// Builds each crate into `<out>/<crate>.o`. The crates are separate programs that share only
/// the workspace, so each one sees the symbols of its own rlib and of the rlibs that belong to
/// none of them: a symbol from another crate's rlib would not be there at link time.
fn build_batch(db: &RootDatabase, args: Args) -> anyhow::Result<()> {
    let (out, report) = (args.out.clone(), args.report.clone().context("a batch needs --report")?);
    let crates: Vec<_> =
        std::iter::once(args.krate.clone()).chain(args.batch.iter().cloned()).collect();
    let rlib_of = |file: &Path| {
        let name = file.file_name()?.to_str()?.strip_prefix("lib")?;
        let name = name.rsplit_once('-').map_or(name, |(name, _)| name);
        crates.iter().find(|krate| krate.as_str() == name)
    };
    for krate in &crates {
        let symbol_files: Vec<_> = args
            .symbol_files
            .iter()
            .filter(|file| rlib_of(file).is_none_or(|owner| owner == krate))
            .cloned()
            .collect();
        let table = &SymbolTable::load(&symbol_files, &args.root()?)?;
        let args = Args {
            project: args.project.clone(),
            krate: krate.clone(),
            batch: Vec::new(),
            out: out.join(format!("{krate}.o")),
            symbol_files: Vec::new(),
            watch: false,
            link_cmd: None,
            patch: None,
            report: Some(report.join(format!("{krate}.tsv"))),
            deny: None,
            isolate: None,
            cfg_test: args.cfg_test,
            features: args.features.clone(),
        };
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            build(db, table, &args, &mut Rejections::default())
        }));
        let error = match built {
            Ok(Ok(_)) => continue,
            Ok(Err(error)) => format!("error: {error:#}"),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("panic");
                format!("panicked: {message}")
            }
        };
        std::fs::write(report.join(format!("{krate}.error")), error)?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    hir_def::nameres::EXPAND_BUILTIN_DERIVES.store(true, std::sync::atomic::Ordering::Relaxed);
    project_model::SYSROOT_AS_BUILT.store(true, std::sync::atomic::Ordering::Relaxed);
    let start = Instant::now();
    let table = SymbolTable::load(&args.symbol_files, &args.root()?)?;
    eprintln!("symbols: {} in {:.1?}", table.len(), start.elapsed());

    let start = Instant::now();
    let cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        set_test: args.cfg_test,
        features: CargoFeatures::Selected {
            features: args.features.clone(),
            no_default_features: false,
        },
        ..Default::default()
    };
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
        num_worker_threads: 1,
        proc_macro_processes: 1,
    };
    let (mut db, vfs, _proc_macro) =
        load_workspace_at(&args.project, &cargo_config, &load_config, &|_| {})?;
    eprintln!("load workspace: {:.1?}", start.elapsed());

    if args.out.is_dir() {
        return build_batch(&db, args);
    }

    let start = Instant::now();
    let resolved = resolve_crates(&db, &args)?;
    eprintln!("resolve: {resolved} crates in {:.1?}", start.elapsed());

    anyhow::ensure!(args.patch.is_none() || args.isolate.is_none(), "--patch with --isolate");
    let report_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !PROBING.get() {
            report_panic(info);
        }
    }));
    let mut known = Rejections::default();
    known.patching = args.patch.is_some();
    let cold = build(&db, &table, &args, &mut known)?;
    eprint!("cold: ");
    report(&cold);
    report_stats(&known.stats);
    link(&args)?;
    if !args.watch {
        return Ok(());
    }

    let mut files = WatchedFile::sources(&vfs);
    eprintln!("watching {} files", files.len());
    loop {
        WatchedFile::watch_included(&mut files, &mut db);
        std::thread::sleep(Duration::from_millis(5));
        let mut noticed = None;
        // Keep scanning until a pass finds nothing new, so one save of several files is one build.
        // Files are read after that: a timestamp can be that of a write still under way, and
        // the rest of that write may not change it again.
        let mut touched = FxHashSet::default();
        loop {
            let before = touched.len();
            for (index, file) in files.iter_mut().enumerate() {
                let Ok(now) = std::fs::metadata(&file.path).and_then(|m| m.modified()) else {
                    continue;
                };
                if now != file.mtime {
                    file.mtime = now;
                    touched.insert(index);
                    noticed.get_or_insert((Instant::now(), now.elapsed().unwrap_or_default()));
                }
            }
            if touched.len() == before {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let Some((noticed, age)) = noticed else { continue };
        let settled = noticed.elapsed();
        let read = Instant::now();
        let mut change = ChangeWithProcMacros::default();
        let changed = touched.len();
        let mut included = Vec::new();
        for index in touched {
            let WatchedFile { file, path, .. } = &files[index];
            match file {
                Watched::Source(id) => {
                    change.change_file(*id, Some(std::fs::read_to_string(path)?))
                }
                Watched::Included(path) => included.push(path),
            }
        }
        let read = read.elapsed();
        eprintln!("change: {changed} files");
        let start = Instant::now();
        db.apply_change(change);
        for path in included {
            db.reload_included_file(path);
        }
        let apply = start.elapsed();
        let warm = build(&db, &table, &args, &mut known)?;
        let built = start.elapsed();
        eprint!("edit (apply {apply:.1?}): ");
        report(&warm);
        report_stats(&known.stats);
        let reported = start.elapsed();
        link(&args)?;
        let total = start.elapsed();
        if std::env::var_os("RUSTD_STATS").is_some() {
            let [roots, revalidate, compile] = warm.steps;
            let phases = known.stats.phases;
            // In the order they happen, from the save on. Each is a duration of its own.
            let steps = [
                ("notice", age),
                ("settle", settled),
                ("read", read),
                ("apply", apply),
                ("roots", roots),
                ("check-roots", warm.looked.roots),
                ("check-instances", warm.looked.instances),
                ("check-statics", warm.looked.statics),
                ("check-left", warm.looked.left),
                (
                    "check-rest",
                    revalidate.saturating_sub(
                        warm.looked.roots
                            + warm.looked.instances
                            + warm.looked.statics
                            + warm.looked.left,
                    ),
                ),
                ("reach", phases.compile),
                ("resolve", phases.resolve),
                ("define", phases.define),
                ("object", phases.object),
                (
                    "compile-rest",
                    compile.saturating_sub(
                        phases.compile + phases.resolve + phases.define + phases.object,
                    ),
                ),
                ("write", warm.emit),
                ("build-rest", built.saturating_sub(apply + warm.codegen + warm.emit)),
                ("report", reported - built),
                ("link", total - reported),
            ];
            let steps: Vec<String> =
                steps.iter().map(|(name, took)| format!("{name}={}", took.as_micros())).collect();
            eprintln!("timeline-us: {}", steps.join(" "));
        }
        eprintln!("edit-to-binary: {total:.1?}");
    }
}

#[allow(dead_code)]
fn abs(path: &Path) -> anyhow::Result<VfsPath> {
    Ok(VfsPath::from(AbsPathBuf::assert_utf8(std::fs::canonicalize(path)?)))
}

fn report_stats(stats: &hir_ty::mir::codegen::BuildStats) {
    if stats.bodies == 0 {
        return;
    }
    eprintln!(
        "stats: {} of {} functions compiled, {} reused",
        stats.compiled, stats.bodies, stats.reused
    );
    if let Some(patched) = stats.patched {
        eprintln!("patch: {patched} functions");
    }
    if let Some(why) = stats.declined {
        eprintln!("no patch: {why}");
    }
    let phases = stats.phases;
    eprintln!(
        "phases: compile {:.1?}, resolve {:.1?}, define {:.1?}, fallback {:.1?}, object {:.1?}, {} rounds",
        phases.compile, phases.resolve, phases.define, phases.fallback, phases.object, stats.rounds
    );
}
