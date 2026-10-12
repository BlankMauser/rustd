//! Spike: keep a workspace loaded in rust-analyzer's database and compile
//! functions to native code that links against rustc-built artifacts.

mod symbols;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, bail};
use hir::{AsAssocItem, AssocItem, AssocItemContainer, Crate, Module, ModuleDef, db::HirDatabase};
use hir_def::{FunctionId, ModuleDefId, StaticId};
use hir_ty::mir::codegen::{CodegenError, CodegenUnit, Rejections, SymbolResolver};
use rustc_hash::FxHashMap;
use ide_db::{ChangeWithProcMacros, RootDatabase};
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use project_model::{CargoConfig, RustLibSource};
use vfs::{AbsPathBuf, Vfs, VfsPath};

use crate::symbols::SymbolTable;

struct Args {
    project: PathBuf,
    krate: String,
    out: PathBuf,
    symbol_files: Vec<PathBuf>,
    watch: bool,
    link_cmd: Option<String>,
    /// Tab-separated per-function outcome, for the harness.
    report: Option<PathBuf>,
    /// File of function paths to leave to rustc, for bisecting miscompiles.
    deny: Option<PathBuf>,
    /// Directory that gets one object per compiled function, for testing each in isolation.
    isolate: Option<PathBuf>,
}

impl Args {
    fn parse() -> anyhow::Result<Args> {
        let mut args = std::env::args().skip(1);
        let mut project = None;
        let mut krate = None;
        let mut out = None;
        let mut symbol_files = Vec::new();
        let mut watch = false;
        let mut link_cmd = None;
        let mut report = None;
        let mut deny = None;
        let mut isolate = None;
        while let Some(arg) = args.next() {
            let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--crate" => krate = Some(value()?),
                "--out" => out = Some(PathBuf::from(value()?)),
                "--symbols" => symbol_files.push(PathBuf::from(value()?)),
                "--link-cmd" => link_cmd = Some(value()?),
                "--report" => report = Some(PathBuf::from(value()?)),
                "--deny" => deny = Some(PathBuf::from(value()?)),
                "--isolate" => isolate = Some(PathBuf::from(value()?)),
                "--watch" => watch = true,
                _ if project.is_none() => project = Some(PathBuf::from(arg)),
                _ => bail!("unexpected argument {arg}"),
            }
        }
        Ok(Args {
            project: project.context("missing project path")?,
            krate: krate.context("missing --crate")?,
            out: out.context("missing --out")?,
            symbol_files,
            watch,
            link_cmd,
            report,
            deny,
            isolate,
        })
    }
}

struct Resolver<'a> {
    db: &'a RootDatabase,
    table: &'a SymbolTable,
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
                let trait_ =
                    self.item_path(trait_.module(self.db), trait_.name(self.db).as_str())?;
                Some(format!("<{self_ty} as {trait_}>::{}", name.as_str()))
            }
            None => Some(format!("<{self_ty}>::{}", name.as_str())),
        }
    }

    fn item_path(&self, module: Module, name: &str) -> Option<String> {
        let krate = module.krate(self.db).display_name(self.db)?;
        let mut path = krate.crate_name().to_string();
        for segment in module.path_segments(self.db) {
            path.push_str("::");
            path.push_str(segment.as_str());
        }
        path.push_str("::");
        path.push_str(name);
        Some(path)
    }

    /// Only non-generic ADTs for now; anything else has no symbol we can name.
    fn type_path(&self, ty: &hir::Type<'_>) -> Option<String> {
        let adt = ty.as_adt()?;
        if ty.type_arguments().next().is_some() {
            return None;
        }
        self.item_path(adt.module(self.db), adt.name(self.db).as_str())
    }
}

impl SymbolResolver for Resolver<'_> {
    fn function(&self, func: FunctionId) -> Option<String> {
        self.table.get(&self.path(func)?).map(str::to_owned)
    }

    fn static_(&self, id: StaticId) -> Option<String> {
        let ModuleDef::Static(it) = ModuleDef::from(ModuleDefId::StaticId(id)) else {
            return None;
        };
        let path = self.item_path(it.module(self.db), it.name(self.db).as_str())?;
        self.table.get(&path).map(str::to_owned)
    }

    fn path(&self, path: &str) -> Option<String> {
        self.table.get(path).map(str::to_owned)
    }
}

fn functions_of(db: &RootDatabase, krate: &str) -> anyhow::Result<Vec<FunctionId>> {
    let krate = Crate::all(db)
        .into_iter()
        .find(|it| it.display_name(db).is_some_and(|name| name.crate_name().as_str() == krate))
        .with_context(|| format!("crate {krate} not found in workspace"))?;
    let mut modules: Vec<Module> = vec![krate.root_module(db)];
    let mut functions = Vec::new();
    while let Some(module) = modules.pop() {
        modules.extend(module.children(db));
        for def in module.declarations(db) {
            if let Ok(ModuleDefId::FunctionId(id)) = ModuleDefId::try_from(def) {
                functions.push(id);
            }
        }
        for imp in module.impl_defs(db) {
            for item in imp.items(db) {
                if let AssocItem::Function(func) = item
                    && let Ok(func) = FunctionId::try_from(func)
                {
                    functions.push(func);
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
}

fn build(
    db: &RootDatabase,
    table: &SymbolTable,
    args: &Args,
    known: &mut Rejections,
) -> anyhow::Result<Build> {
    let start = Instant::now();
    let resolver = Resolver { db, table };
    let hir_db: &dyn HirDatabase = db;
    let mut roots = functions_of(db, &args.krate)?;
    if let Some(deny) = &args.deny {
        let deny = std::fs::read_to_string(deny)?;
        let deny: rustc_hash::FxHashSet<&str> = deny.lines().collect();
        roots.retain(|&func| resolver.path(func).is_none_or(|path| !deny.contains(path.as_str())));
    }
    // Salsa attaches the database for the duration of the closure; the solver needs it.
    let unit = hir::attach_db(db, || CodegenUnit::compile(hir_db, &resolver, &roots, known))
        .map_err(anyhow::Error::msg)?;
    let name = |func| resolver.path(func).unwrap_or_else(|| "<assoc fn>".to_owned());
    let codegen = start.elapsed();
    let start = Instant::now();
    std::fs::write(&args.out, &unit.object)?;
    let build = Build {
        compiled: unit.compiled.iter().map(|&(func, _)| name(func)).collect(),
        skipped: unit.skipped.into_iter().map(|(func, e)| (name(func), e)).collect(),
        instances: unit.instances,
        codegen,
        emit: start.elapsed(),
    };
    if let Some(path) = &args.report {
        build.write_report(path)?;
    }
    if let Some(dir) = &args.isolate {
        std::fs::create_dir_all(dir)?;
        let mut index = String::new();
        for (i, &(func, _)) in unit.compiled.iter().enumerate() {
            let single = hir::attach_db(db, || CodegenUnit::compile(hir_db, &resolver, &[func], known))
                .map_err(anyhow::Error::msg)?;
            std::fs::write(dir.join(format!("{i}.o")), single.object)?;
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
        writeln!(out, "#\tinstances\t{}\tcodegen_us\t{}", self.instances, self.codegen.as_micros())?;
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

fn source_files(vfs: &Vfs, root: &Path) -> Vec<(vfs::FileId, PathBuf, SystemTime)> {
    vfs.iter()
        .filter_map(|(id, path)| {
            let path: &Path = path.as_path()?.as_ref();
            if !path.starts_with(root) || path.components().any(|c| c.as_os_str() == "target") {
                return None;
            }
            let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
            Some((id, path.to_owned(), mtime))
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    let start = Instant::now();
    let table = SymbolTable::load(&args.symbol_files)?;
    eprintln!("symbols: {} in {:.1?}", table.len(), start.elapsed());

    let start = Instant::now();
    let cargo_config =
        CargoConfig { sysroot: Some(RustLibSource::Discover), ..Default::default() };
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

    let mut known = Rejections::default();
    let cold = build(&db, &table, &args, &mut known)?;
    eprint!("cold: ");
    report(&cold);
    link(&args)?;
    if !args.watch {
        return Ok(());
    }

    let root = std::fs::canonicalize(&args.project)?;
    let mut files = source_files(&vfs, &root);
    eprintln!("watching {} files", files.len());
    loop {
        std::thread::sleep(Duration::from_millis(5));
        let mut change = ChangeWithProcMacros::default();
        let mut changed = false;
        for (id, path, mtime) in &mut files {
            let Ok(now) = std::fs::metadata(&*path).and_then(|m| m.modified()) else { continue };
            if now != *mtime {
                *mtime = now;
                change.change_file(*id, Some(std::fs::read_to_string(&*path)?));
                changed = true;
            }
        }
        if !changed {
            continue;
        }
        let start = Instant::now();
        db.apply_change(change);
        let apply = start.elapsed();
        let warm = build(&db, &table, &args, &mut known)?;
        eprint!("edit (apply {apply:.1?}): ");
        report(&warm);
        link(&args)?;
        eprintln!("edit-to-binary: {:.1?}", start.elapsed());
    }
}

#[allow(dead_code)]
fn abs(path: &Path) -> anyhow::Result<VfsPath> {
    Ok(VfsPath::from(AbsPathBuf::assert_utf8(std::fs::canonicalize(path)?)))
}
