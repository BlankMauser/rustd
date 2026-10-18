//! The cargo that runs the wrapper. Cargo tells the rustc it runs nothing of its own command
//! line, and which build a unit is of is read from it: the command, its flags, the directory
//! it runs in and its environment.

use std::{ffi::OsString, path::PathBuf};

/// A cargo that is running, as the system describes it.
pub(crate) struct Cargo {
    pub(crate) pid: u32,
    pub(crate) executable: PathBuf,
    /// Its arguments, without its own name.
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
}

/// How many processes up from the wrapper cargo is looked for: there are the wrappers the
/// user chains between the two, and nothing else.
const FARTHEST: usize = 8;

impl Cargo {
    /// The nearest process above this one that is cargo. `Err` is why none is known.
    pub(crate) fn above() -> Result<Cargo, String> {
        let mut pid = std::process::id();
        for _ in 0..FARTHEST {
            pid = system::parent(pid).ok_or("the process that runs rustc is not cargo")?;
            if let Some(cargo) = system::described(pid).filter(Cargo::is_cargo) {
                return Ok(cargo);
            }
        }
        Err("the process that runs rustc is not cargo".to_owned())
    }

    fn is_cargo(&self) -> bool {
        self.executable.file_stem().is_some_and(|it| it == "cargo")
    }
}

#[cfg(target_os = "linux")]
mod system {
    use std::{
        ffi::OsString,
        os::unix::ffi::OsStringExt as _,
        path::{Path, PathBuf},
    };

    use super::Cargo;

    fn of(pid: u32, file: &str) -> PathBuf {
        Path::new("/proc").join(pid.to_string()).join(file)
    }

    /// The process that started `pid`, when `pid` runs.
    pub(super) fn parent(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(of(pid, "stat")).ok()?;
        // `pid (name) state ppid ...`, and the name may have spaces and brackets of its own.
        let after_name = &stat[stat.rfind(')')? + 1..];
        let mut fields = after_name.split_whitespace();
        let state = fields.next()?;
        let parent = fields.next()?.parse().ok()?;
        // A process that ended and is not reaped yet.
        (state != "Z").then_some(parent)
    }

    fn words(pid: u32, file: &str) -> Option<Vec<OsString>> {
        let bytes = std::fs::read(of(pid, file)).ok()?;
        let words = bytes.split(|it| *it == 0).filter(|it| !it.is_empty());
        Some(words.map(|it| OsString::from_vec(it.to_vec())).collect())
    }

    pub(super) fn described(pid: u32) -> Option<Cargo> {
        let args = words(pid, "cmdline")?.into_iter().skip(1);
        let env = words(pid, "environ")?.into_iter().filter_map(|it| {
            let bytes = it.into_vec();
            let at = bytes.iter().position(|it| *it == b'=')?;
            let (name, value) = (bytes[..at].to_vec(), bytes[at + 1..].to_vec());
            Some((OsString::from_vec(name), OsString::from_vec(value)))
        });
        Some(Cargo {
            pid,
            executable: std::fs::read_link(of(pid, "exe")).ok()?,
            args: args.map(|it| it.into_string().ok()).collect::<Option<_>>()?,
            cwd: std::fs::read_link(of(pid, "cwd")).ok()?,
            env: env.collect(),
        })
    }
}

/// Elsewhere the command line of another process is not read yet, and every build under
/// plain cargo is rustc's.
#[cfg(not(target_os = "linux"))]
mod system {
    use super::Cargo;

    pub(super) fn parent(_pid: u32) -> Option<u32> {
        None
    }

    pub(super) fn described(_pid: u32) -> Option<Cargo> {
        None
    }
}
