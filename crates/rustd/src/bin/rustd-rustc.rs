//! What cargo is told to run rustc through for the crates of a workspace: cargo runs
//! `rustd-rustc rustc ...`, and it becomes `rustd rustc-wrapper rustc ...`, the `rustd`
//! being the one installed next to it. Cargo's configuration names a wrapper by its path
//! alone, so the wrapper has a path of its own.
//!
//! On Windows it is also what cargo runs in the place of a build script of the workspace,
//! linked there under the script's name: it becomes `rustd build-script` for the script,
//! which was moved aside as `<name>.exe.real`, with the `rustd` named in `<name>.exe.rustd`.

use std::process::{Command, ExitCode};

pub fn main() -> ExitCode {
    #[cfg(windows)]
    if let Some(ended) = as_build_script() {
        return ended;
    }
    let mut asked = std::env::args_os().skip(1);
    let rustd = std::env::current_exe()
        .map(|this| this.with_file_name(format!("rustd{}", std::env::consts::EXE_SUFFIX)));
    let why = match rustd {
        Ok(rustd) => {
            let mut command = Command::new(&rustd);
            command.arg("rustc-wrapper").args(std::env::args_os().skip(1));
            match ended(&mut command) {
                Ok(code) => return code,
                Err(error) => format!("{} does not run ({error})", rustd.display()),
            }
        }
        Err(error) => format!("rustd-rustc does not know where it is installed ({error})"),
    };
    // The build is the user's, with rustd or without.
    eprintln!("rustd: {why}; rustc compiles");
    let Some(rustc) = asked.next() else { return ExitCode::FAILURE };
    match Command::new(rustc).args(asked).status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("error: could not run rustc: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Becomes the command, where a process can.
#[cfg(unix)]
fn ended(command: &mut Command) -> std::io::Result<ExitCode> {
    Err(std::os::unix::process::CommandExt::exec(command))
}

#[cfg(not(unix))]
fn ended(command: &mut Command) -> std::io::Result<ExitCode> {
    Ok(ExitCode::from(command.status()?.code().unwrap_or(1) as u8))
}

/// Runs the build script this executable stands in front of, when it runs under the name of
/// one: the name rustc linked the script under, or the one cargo gives it in the same
/// directory.
#[cfg(windows)]
fn as_build_script() -> Option<ExitCode> {
    let this = std::env::current_exe().ok()?;
    let name = this.file_stem()?.to_str()?;
    if !name.starts_with("build_script_") && !name.starts_with("build-script-") {
        return None;
    }
    let mut beside = std::fs::read_dir(this.parent()?).ok()?.flatten().map(|it| it.path());
    let real = beside.find(|it| it.to_str().is_some_and(|it| it.ends_with(".exe.real")))?;
    let args = || std::env::args_os().skip(1);
    let through = std::fs::read_to_string(real.with_extension("rustd")).and_then(|rustd| {
        Command::new(rustd)
            .arg("build-script")
            .args(args())
            .env("RUSTD_BUILD_SCRIPT", &real)
            .status()
    });
    // Without rustd the script still runs; a change it reruns for is then cargo's to build.
    match through.or_else(|_| Command::new(&real).args(args()).status()) {
        Ok(status) => Some(ExitCode::from(status.code().unwrap_or(1) as u8)),
        Err(error) => {
            eprintln!("error: could not run the build script {}: {error}", real.display());
            Some(ExitCode::FAILURE)
        }
    }
}
