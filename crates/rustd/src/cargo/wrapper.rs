//! rustd as cargo's `RUSTC_WORKSPACE_WRAPPER` under `cargo rustd`: rustc runs as cargo asked,
//! and what it built of a crate of the workspace is recorded for the daemon. A crate that is
//! linked is also asked for its link line and for the objects it was linked from.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context as _;

use super::{
    script,
    units::{Copied, Record, stamp_now},
};
use crate::stub::VALUED;

/// The variable that makes `rustd` this wrapper.
pub(crate) const ENABLED: &str = "RUSTD_CARGO_WRAPS";

/// Runs rustc, records the unit when rustc succeeded, and ends as rustc did.
pub(crate) fn command(mut args: impl Iterator<Item = OsString>) -> anyhow::Result<()> {
    let rustc = args.next().context("cargo names rustc first")?;
    let args: Vec<OsString> = args.collect();
    let Some(unit) = Unit::of(&args) else {
        let status = Command::new(&rustc).args(&args).status()?;
        std::process::exit(status.code().unwrap_or(1));
    };
    let stamp = stamp_now();
    let copies = unit.records.join(&unit.id).join(stamp.to_string());
    let line = copies.join("link.line");
    let mut command = Command::new(&rustc);
    command.args(&args);
    if unit.linked {
        std::fs::create_dir_all(&copies)?;
        command.arg(format!("--print=link-args={}", line.display())).arg("-Csave-temps");
    }
    let status = command.status().with_context(|| format!("running {}", Path::new(&rustc).display()))?;
    if status.success()
        && let Err(error) = unit.record(stamp, &copies, unit.linked.then_some(line))
    {
        // The build is good; the daemon has no part in this crate until it is built again.
        eprintln!("warning: rustd did not record `{}`: {error:#}", unit.id);
    }
    std::process::exit(status.code().unwrap_or(1));
}

/// A run of rustc that compiles a crate, as cargo's arguments describe it.
struct Unit {
    id: String,
    crate_name: String,
    args: Vec<String>,
    source: PathBuf,
    cwd: PathBuf,
    records: PathBuf,
    /// Whether rustc links a program of the workspace from it.
    linked: bool,
    /// Whether rustc links a build script from it.
    script: bool,
}

impl Unit {
    /// `None` for a run that compiles nothing to record: a question to rustc (`--print`,
    /// `-vV`), source on standard input, or arguments that are not text.
    fn of(args: &[OsString]) -> Option<Unit> {
        let args: Vec<String> =
            args.iter().map(|it| it.to_str().map(str::to_owned)).collect::<Option<_>>()?;
        let value = |flag: &str| {
            let at = args.iter().position(|it| it == flag)?;
            args.get(at + 1).cloned()
        };
        let crate_name = value("--crate-name")?;
        let out_dir = value("--out-dir")?;
        if args.iter().any(|it| it == "-" || it.starts_with("--print")) {
            return None;
        }
        // The one argument that is a path to a Rust file and not the value of a flag.
        let source = (0..args.len()).find(|&at| {
            args[at].ends_with(".rs") && !(at > 0 && VALUED.contains(&args[at - 1].as_str()))
        })?;
        let cwd = std::env::current_dir().ok()?;
        let mut unit = Unit {
            id: String::new(),
            crate_name,
            source: cwd.join(&args[source]),
            records: Record::dir_for(&cwd.join(out_dir)),
            args,
            cwd,
            linked: false,
            script: false,
        };
        let record = unit.as_record(0, Vec::new(), None);
        let emits_link = unit.args.iter().any(|it| {
            it.strip_prefix("--emit=").is_some_and(|kinds| kinds.split(',').any(|it| it == "link"))
        });
        unit.script = record.is_build_script() && emits_link;
        unit.linked = record.is_linked() && emits_link && !record.runs_in_build();
        unit.id = format!("{}{}", unit.crate_name, record.codegen("extra-filename").unwrap_or_default());
        Some(unit)
    }

    fn as_record(&self, stamp: u128, copies: Vec<Copied>, link_line: Option<PathBuf>) -> Record {
        Record {
            id: self.id.clone(),
            crate_name: self.crate_name.clone(),
            cwd: self.cwd.clone(),
            args: self.args.clone(),
            source: self.source.clone(),
            stamp,
            copies,
            link_line,
        }
    }

    /// Copies what a later build of the unit writes over and writes the record.
    fn record(&self, stamp: u128, to: &Path, link_line: Option<PathBuf>) -> anyhow::Result<()> {
        let record = self.as_record(stamp, Vec::new(), link_line.clone());
        // The executable is the daemon's to link; of the outputs it reads the libraries.
        let libraries = record.outputs().into_iter().filter(|it| it.extension().is_some_and(|it| it == "rlib"));
        let mut originals: Vec<PathBuf> = libraries.filter(|it| it.exists()).collect();
        if let Some(line) = &link_line {
            let line = std::fs::read_to_string(line)
                .with_context(|| format!("rustc wrote no link line to {}", line.display()))?;
            let args = shlex::split(&line).context("the link line is not quoted as a shell's")?;
            let objects = args.into_iter().filter(|it| it.ends_with(".rcgu.o")).map(PathBuf::from);
            originals.extend(objects.filter(|it| it.exists()));
        }
        std::fs::create_dir_all(to)?;
        let mut copies = Vec::new();
        for original in originals {
            let name = original.file_name().context("an output without a name")?;
            let copy = to.join(name);
            std::fs::copy(&original, &copy)
                .with_context(|| format!("copying {}", original.display()))?;
            copies.push(Copied { original, copy });
        }
        if self.script
            && let Some(linked) = record.outputs().first()
        {
            script::stand_in_front(linked)?;
        }
        self.as_record(stamp, copies, link_line).write(&self.records)
    }
}
