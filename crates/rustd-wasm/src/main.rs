//! The compiler of the browser run from a shell, to try it without one, and to pack the
//! source of `core` for the page.
//!
//! ```text
//! rustd-wasm bundle <sysroot>/lib/rustlib/src/rust/library core.bundle
//! rustd-wasm build core.bundle out/ first.rs [edited.rs ...]
//! ```
//!
//! `build` writes `out/<n>.wasm` and `out/<n>.json` for each source in turn, as edits of
//! one program.

use std::{error::Error, fs, path::Path};

use rustd_wasm::{Compiler, MAIN, bundle};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        ["bundle", library, out] => {
            let mut files = Vec::new();
            for (dir, only) in [
                ("core/src", None),
                ("stdarch/crates/core_arch/src", Some("wasm32")),
                ("portable-simd/crates/core_simd/src", None),
            ] {
                collect(Path::new(library), Path::new(dir), only, &mut files)?;
            }
            fs::write(out, bundle::pack(&files))?;
        }
        ["build", core, out, ref sources @ ..] => {
            let mut compiler = Compiler::default();
            compiler.set_files(bundle::unpack(&fs::read(core)?)?);
            fs::create_dir_all(out)?;
            for (at, source) in sources.iter().enumerate() {
                compiler.set_file(MAIN, fs::read_to_string(source)?);
                let build = compiler.build()?;
                fs::write(format!("{out}/{at}.wasm"), &build.patch.module)?;
                fs::write(format!("{out}/{at}.json"), build.report())?;
                eprintln!("{source}: {}", build.report());
            }
        }
        _ => {
            return Err(
                "usage: rustd-wasm bundle <library> <out> | build <bundle> <out> <rs>..".into()
            );
        }
    }
    Ok(())
}

/// The Rust files under `library/dir`, named as the compiler's file system has them. With
/// `only`, of the directories right under it only that one.
fn collect(
    library: &Path,
    dir: &Path,
    only: Option<&str>,
    files: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(library.join(dir))? {
        let entry = entry?;
        let path = dir.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if only.is_none_or(|only| entry.file_name() == only) {
                collect(library, &path, None, files)?;
            }
        } else if path.extension().is_some_and(|it| it == "rs" || it == "md") {
            files.push((format!("/{}", path.display()), fs::read_to_string(entry.path())?));
        }
    }
    Ok(())
}
