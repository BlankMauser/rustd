//! Maps Rust paths to the mangled names rustc gave them, read from rlibs and objects.

use std::path::{Path, PathBuf};

use anyhow::Context;
use object::{Object, ObjectSymbol, read::archive::ArchiveFile};
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;

#[derive(Default)]
pub(crate) struct SymbolTable {
    by_path: FxHashMap<String, String>,
    /// Symbols of items nested in closures and anonymous constants, by their path without the
    /// `{closure#0}`-like segments, with the numbers of the closures among those.
    nested: FxHashMap<String, Vec<NestedSymbol>>,
    /// rustc's `StableCrateId` of each crate named in a symbol. `None` when two crates share
    /// the name, two versions of one package say.
    crates: FxHashMap<String, Option<u64>>,
    /// The `StableCrateId` of the crate whose source starts at a file, for the crates that
    /// share a name. Their paths keep the `name[id]` rustc mangled into the symbol.
    by_root: FxHashMap<PathBuf, u64>,
}

struct NestedSymbol {
    name: String,
    closures: Vec<u32>,
}

/// A linkable symbol: its mangled name and its path with and without crate ids.
struct Symbol {
    name: String,
    with_ids: String,
    path: String,
}

impl SymbolTable {
    /// `root` is what the paths in cargo's dep-info files are relative to.
    pub(crate) fn load(files: &[PathBuf], root: &Path) -> anyhow::Result<SymbolTable> {
        let mut table = SymbolTable::default();
        let mut symbols = Vec::new();
        let mut owners = Vec::new();
        for file in files {
            let data =
                std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
            let first = symbols.len();
            match ArchiveFile::parse(&*data) {
                Ok(archive) => {
                    for member in archive.members() {
                        table.add_object(member?.data(&*data)?, &mut symbols);
                    }
                }
                Err(_) => table.add_object(&data, &mut symbols),
            }
            if let Some((name, source)) = crate_root(file, root) {
                owners.push((name, source, first..symbols.len()));
            }
        }
        for (name, source, range) in owners {
            if table.crates.get(&name) != Some(&None) {
                continue;
            }
            // Nothing a crate defines mentions the other crate of its name.
            let id = symbols[range].iter().find_map(|symbol| {
                crate_ids(&symbol.with_ids).find(|it| it.0 == name).map(|it| it.1)
            });
            if let Some(id) = id {
                table.by_root.insert(source, id);
            }
        }
        let mut ambiguous = Vec::new();
        for Symbol { name, with_ids, path } in symbols {
            let shared = |krate: &str| table.crates.get(krate) == Some(&None);
            let path = if crate_ids(&with_ids).any(|(krate, _)| shared(krate)) {
                without_ids(&with_ids, |krate| !shared(krate))
            } else {
                path
            };
            if path.contains("::{") {
                let named: Vec<_> = path.split("::").filter(|it| !it.starts_with('{')).collect();
                let closures = path
                    .split("::")
                    .filter_map(|it| it.strip_prefix("{closure#")?.strip_suffix('}')?.parse().ok())
                    .collect();
                let symbols = table.nested.entry(named.join("::")).or_default();
                if symbols.iter().all(|it| it.name != name) {
                    symbols.push(NestedSymbol { name: name.clone(), closures });
                }
            }
            match table.by_path.entry(path) {
                Entry::Vacant(entry) => {
                    entry.insert(name);
                }
                Entry::Occupied(entry) => {
                    if without_instantiating_crate(entry.get())
                        != without_instantiating_crate(&name)
                    {
                        ambiguous.push(entry.key().clone());
                    }
                }
            }
        }
        // Items in two `const _` blocks of a module print as one path. Linking either to the
        // other's symbol is a miscompile, so neither has one.
        for path in ambiguous {
            table.by_path.remove(&path);
        }
        Ok(table)
    }

    fn add_object(&mut self, data: &[u8], symbols: &mut Vec<Symbol>) {
        // rlibs also hold `lib.rmeta`, which isn't an object file.
        let Ok(object) = object::File::parse(data) else { return };
        for symbol in object.symbols() {
            // A reference is as good as a definition: something in the link defines it.
            // Not `is_definition`, which leaves out thread locals.
            let defined = symbol.section_index().is_some() || symbol.is_common();
            let linkable = symbol.is_undefined() || (defined && symbol.is_global());
            if !linkable {
                continue;
            }
            let Ok(name) = symbol.name() else { continue };
            // The memory a static points to is a symbol that demangles to the static's path.
            if name.starts_with("_RNn") {
                continue;
            }
            let Ok(demangled) = rustc_demangle::try_demangle(name) else { continue };
            let with_ids = demangled.to_string();
            for (krate, id) in crate_ids(&with_ids) {
                match self.crates.get_mut(krate) {
                    Some(known) if *known != Some(id) => *known = None,
                    Some(_) => {}
                    None => {
                        self.crates.insert(krate.to_owned(), Some(id));
                    }
                }
            }
            // `{:#}` drops the crate disambiguator hashes.
            symbols.push(Symbol {
                name: name.to_owned(),
                with_ids,
                path: format!("{demangled:#}"),
            });
        }
    }

    /// How paths spell the crate named `name` whose source starts at `root`: `name[id]` when
    /// another crate has the name too.
    pub(crate) fn crate_path(&self, name: &str, root: &Path) -> Option<String> {
        match self.crates.get(name) {
            Some(None) => Some(format!("{name}[{:x}]", self.by_root.get(root)?)),
            _ => Some(name.to_owned()),
        }
    }

    pub(crate) fn stable_crate_id(&self, name: &str, root: &Path) -> Option<u64> {
        match *self.crates.get(name)? {
            Some(id) => Some(id),
            None => self.by_root.get(root).copied(),
        }
    }

    pub(crate) fn get(&self, path: &str) -> Option<&str> {
        self.by_path.get(path).map(String::as_str)
    }

    /// The symbol of an item whose path has unnamed segments between the named ones given.
    ///
    /// `closures` are the numbers rustc gives the closures around the item, outermost first,
    /// which tell apart the items that only differ in those. Anonymous constants are not
    /// numbered, so two items that differ in one of those have no symbol here.
    pub(crate) fn get_nested(&self, named: &str, closures: &[u32]) -> Option<&str> {
        let mut symbols = match &**self.nested.get(named)? {
            [only] => return Some(&only.name),
            symbols => symbols.iter().filter(|it| it.closures == closures),
        };
        let symbol = symbols.next()?;
        symbols.next().is_none().then_some(&*symbol.name)
    }

    pub(crate) fn len(&self) -> usize {
        self.by_path.len()
    }
}

/// The name of the crate in `libname-hash.rlib`, or in the `name-hash.<unit>.rcgu.o` of a crate
/// built as a test binary, and the file its source starts at, which the dep-info file cargo
/// leaves next to it lists first.
fn crate_root(rlib: &Path, root: &Path) -> Option<(String, PathBuf)> {
    let file = rlib.file_name()?.to_str()?;
    let stem = match file.strip_suffix(".rlib") {
        Some(stem) => stem.strip_prefix("lib")?,
        None => file.strip_suffix(".rcgu.o")?.split_once('.')?.0,
    };
    let (name, _) = stem.rsplit_once('-')?;
    let dep_info = std::fs::read_to_string(rlib.with_file_name(format!("{stem}.d"))).ok()?;
    let (_, sources) = dep_info.lines().next()?.split_once(": ")?;
    // FIXME: a path with a space in it is escaped.
    let source = sources.split(' ').next()?;
    Some((name.to_owned(), root.join(source)))
}

/// A v0 symbol without the crate that instantiated it, which ends the symbol of a generic
/// instance as `Cs<id>_<len><name>` or as a back reference `B<offset>_`. Copies of one instance
/// made by two crates differ only in that.
fn without_instantiating_crate(symbol: &str) -> &str {
    let is_base62 = |c: char| c.is_ascii_alphanumeric();
    if let Some(body) = symbol.strip_suffix('_')
        && let Some(start) = body.rfind('B')
        && body[start + 1..].chars().all(is_base62)
    {
        return &symbol[..start];
    }
    // `Cs<id>_<len><name>`: the name is the last `len` bytes.
    let Some(crate_start) = symbol.rfind("Cs") else { return symbol };
    let Some((id, ident)) = symbol[crate_start + 2..].split_once('_') else { return symbol };
    let digits = ident.chars().take_while(char::is_ascii_digit).count();
    let named = ident[..digits].parse() == Ok(ident.len() - digits);
    if id.chars().all(is_base62) && named { &symbol[..crate_start] } else { symbol }
}

/// The `name[id]` a demangled v0 symbol prints for each crate in it.
fn crate_ids(demangled: &str) -> impl Iterator<Item = (&str, u64)> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut rest = demangled;
    std::iter::from_fn(move || {
        loop {
            let (before, after) = rest.split_once('[')?;
            rest = after;
            let Some((id, _)) = after.split_once(']') else { continue };
            let name_len = before.chars().rev().take_while(|&c| is_ident(c)).count();
            let name = &before[before.len() - name_len..];
            let Ok(id) = u64::from_str_radix(id, 16) else { continue };
            if !name.is_empty() {
                return Some((name, id));
            }
        }
    })
}

/// The demangled symbol without the ids of the crates `drop` picks.
fn without_ids(demangled: &str, drop: impl Fn(&str) -> bool) -> String {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut path = String::with_capacity(demangled.len());
    let mut rest = demangled;
    while let Some(open) = rest.find('[') {
        let (before, after) = rest.split_at(open);
        path.push_str(before);
        let name_len = before.chars().rev().take_while(|&c| is_ident(c)).count();
        let name = &before[before.len() - name_len..];
        match after.find(']') {
            Some(close)
                if !name.is_empty() && u64::from_str_radix(&after[1..close], 16).is_ok() =>
            {
                if !drop(name) {
                    path.push_str(&after[..=close]);
                }
                rest = &after[close + 1..];
            }
            _ => {
                path.push('[');
                rest = &after[1..];
            }
        }
    }
    path.push_str(rest);
    path
}
