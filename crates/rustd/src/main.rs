//! Spike: keep a workspace loaded in rust-analyzer's database and compile
//! functions to native code that links against rustc-built artifacts.

mod symbols;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, bail};
use hir::{AsAssocItem, Crate, Module, ModuleDef, db::HirDatabase};
use hir_def::{FunctionId, ModuleDefId};
use hir_ty::mir::codegen::{CodegenError, ObjectCodegen, OverflowOp, SymbolResolver};
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
        while let Some(arg) = args.next() {
            let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--crate" => krate = Some(value()?),
                "--out" => out = Some(PathBuf::from(value()?)),
                "--symbols" => symbol_files.push(PathBuf::from(value()?)),
                "--link-cmd" => link_cmd = Some(value()?),
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
        })
    }
}

struct Resolver<'a> {
    db: &'a RootDatabase,
    table: &'a SymbolTable,
}

impl Resolver<'_> {
    fn path(&self, func: FunctionId) -> Option<String> {
        let ModuleDef::Function(func) = ModuleDef::from(ModuleDefId::FunctionId(func)) else {
            return None;
        };
        // Associated functions need `<Type>::name` / `<Type as Trait>::name` paths.
        if func.as_assoc_item(self.db).is_some() {
            return None;
        }
        let module = func.module(self.db);
        let krate = module.krate(self.db).display_name(self.db)?;
        let mut path = krate.crate_name().to_string();
        for segment in module.path_segments(self.db) {
            path.push_str("::");
            path.push_str(segment.as_str());
        }
        path.push_str("::");
        path.push_str(func.name(self.db).as_str());
        Some(path)
    }
}

impl SymbolResolver for Resolver<'_> {
    fn function(&self, func: FunctionId) -> Option<String> {
        self.table.get(&self.path(func)?).map(str::to_owned)
    }

    fn overflow_panic(&self, op: OverflowOp) -> Option<String> {
        let name = match op {
            OverflowOp::Add => "add",
            OverflowOp::Sub => "sub",
            OverflowOp::Mul => "mul",
            OverflowOp::Div => "div",
            OverflowOp::Rem => "rem",
            OverflowOp::Neg => "neg",
            OverflowOp::Shl => "shl",
            OverflowOp::Shr => "shr",
        };
        let path = format!("core::panicking::panic_const::panic_const_{name}_overflow");
        self.table.get(&path).map(str::to_owned)
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
    }
    Ok(functions)
}

struct Build {
    compiled: Vec<String>,
    skipped: Vec<(String, CodegenError)>,
    codegen: Duration,
    emit: Duration,
}

fn build(db: &RootDatabase, table: &SymbolTable, args: &Args) -> anyhow::Result<Build> {
    let start = Instant::now();
    let resolver = Resolver { db, table };
    let hir_db: &dyn HirDatabase = db;
    let mut build =
        Build { compiled: vec![], skipped: vec![], codegen: Duration::ZERO, emit: Duration::ZERO };
    let mut codegen = ObjectCodegen::new(hir_db, &resolver).map_err(anyhow::Error::msg)?;
    for func in functions_of(db, &args.krate)? {
        let name = resolver.path(func).unwrap_or_else(|| "<assoc fn>".to_owned());
        // Salsa attaches the database for the duration of the closure; the solver needs it.
        match hir::attach_db(db, || codegen.define_function(func)) {
            Ok(_) => build.compiled.push(name),
            Err(e) => build.skipped.push((name, e)),
        }
    }
    build.codegen = start.elapsed();
    let start = Instant::now();
    let object = codegen.finish().map_err(anyhow::Error::msg)?;
    std::fs::write(&args.out, object)?;
    build.emit = start.elapsed();
    Ok(build)
}

fn report(build: &Build) {
    eprintln!(
        "compiled {} fns in {:.1?} (+{:.1?} emit): {}",
        build.compiled.len(),
        build.codegen,
        build.emit,
        build.compiled.join(", ")
    );
    for (name, error) in &build.skipped {
        eprintln!("  left to rustc: {name}: {error}");
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

    let cold = build(&db, &table, &args)?;
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
        let warm = build(&db, &table, &args)?;
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
