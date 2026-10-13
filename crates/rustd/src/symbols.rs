//! Maps Rust paths to the mangled names rustc gave them, read from rlibs and objects.

use std::path::PathBuf;

use anyhow::Context;
use object::{Object, ObjectSymbol, read::archive::ArchiveFile};
use rustc_hash::FxHashMap;

#[derive(Default)]
pub(crate) struct SymbolTable {
    by_path: FxHashMap<String, String>,
    /// Symbols of items nested in closures and anonymous constants, by their path without the
    /// `{closure#0}`-like segments, which we can't number the way rustc does. `None` when
    /// several symbols share such a path.
    nested: FxHashMap<String, Option<String>>,
}

impl SymbolTable {
    pub(crate) fn load(files: &[PathBuf]) -> anyhow::Result<SymbolTable> {
        let mut table = SymbolTable::default();
        for file in files {
            let data =
                std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
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
            // A reference is as good as a definition: something in the link defines it.
            let linkable = symbol.is_undefined() || (symbol.is_definition() && symbol.is_global());
            if !linkable {
                continue;
            }
            let Ok(name) = symbol.name() else { continue };
            let Ok(demangled) = rustc_demangle::try_demangle(name) else { continue };
            // `{:#}` drops the crate disambiguator hashes.
            let path = format!("{demangled:#}");
            if path.contains("::{") {
                let named: Vec<_> = path.split("::").filter(|it| !it.starts_with('{')).collect();
                self.nested
                    .entry(named.join("::"))
                    .and_modify(|it| {
                        if it.as_deref() != Some(name) {
                            *it = None;
                        }
                    })
                    .or_insert_with(|| Some(name.to_owned()));
            }
            self.by_path.entry(path).or_insert_with(|| name.to_owned());
        }
    }

    pub(crate) fn get(&self, path: &str) -> Option<&str> {
        self.by_path.get(path).map(String::as_str)
    }

    /// The symbol of an item whose path has unnamed segments between the named ones given.
    pub(crate) fn get_nested(&self, named: &str) -> Option<&str> {
        self.nested.get(named)?.as_deref()
    }

    pub(crate) fn len(&self) -> usize {
        self.by_path.len()
    }
}
