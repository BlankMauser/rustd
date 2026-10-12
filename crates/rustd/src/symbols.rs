//! Maps Rust paths to the mangled names rustc gave them, read from rlibs and objects.

use std::path::PathBuf;

use anyhow::Context;
use object::{Object, ObjectSymbol, read::archive::ArchiveFile};
use rustc_hash::FxHashMap;

#[derive(Default)]
pub(crate) struct SymbolTable {
    by_path: FxHashMap<String, String>,
}

impl SymbolTable {
    pub(crate) fn load(files: &[PathBuf]) -> anyhow::Result<SymbolTable> {
        let mut table = SymbolTable::default();
        for file in files {
            let data = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
            match ArchiveFile::parse(&*data) {
                Ok(archive) => {
                    for member in archive.members() {
                        table.add_object(member?.data(&*data)?);
                    }
                }
                Err(_) => table.add_object(&data),
            }
        }
        Ok(table)
    }

    fn add_object(&mut self, data: &[u8]) {
        // rlibs also hold `lib.rmeta`, which isn't an object file.
        let Ok(object) = object::File::parse(data) else { return };
        for symbol in object.symbols() {
            if !symbol.is_definition() || !symbol.is_global() {
                continue;
            }
            let Ok(name) = symbol.name() else { continue };
            let Ok(demangled) = rustc_demangle::try_demangle(name) else { continue };
            // `{:#}` drops the crate disambiguator hashes.
            self.by_path.entry(format!("{demangled:#}")).or_insert_with(|| name.to_owned());
        }
    }

    pub(crate) fn get(&self, path: &str) -> Option<&str> {
        self.by_path.get(path).map(String::as_str)
    }

    pub(crate) fn len(&self) -> usize {
        self.by_path.len()
    }
}
