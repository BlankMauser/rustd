//! The examples in the documentation of a crate, found as rustdoc finds them: every item with
//! documentation is asked for its Markdown, and every code block of it that is Rust is one.

use std::path::PathBuf;

use hir::{Adt, AssocItem, HasAttrs, HasSource, Module, ModuleDef};
use ide_db::{RootDatabase, base_db::SourceDatabase, rust_doc::is_rust_fence};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use syntax::{AstNode, TextRange, TextSize};

/// One code block of the documentation that is a test.
pub(crate) struct Example {
    /// The path of the documented item in its crate: empty for the crate itself.
    pub(crate) item: String,
    /// The file the documentation is written in.
    pub(crate) file: PathBuf,
    /// The line the block starts on, as rustdoc counts it.
    pub(crate) line: usize,
    pub(crate) marks: Marks,
    /// The code with its hidden lines as code.
    pub(crate) code: String,
}

/// What the line that opens a block says of it.
#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub(crate) struct Marks {
    /// `ignore`, or `ignore-` and a part of the target's triple, which are in `ignored_on`.
    pub(crate) ignore: bool,
    pub(crate) ignored_on: Vec<String>,
    pub(crate) no_run: bool,
    pub(crate) should_panic: bool,
    pub(crate) compile_fail: bool,
    /// Whether the block has its own `#[test]` functions for the test harness to run.
    pub(crate) test_harness: bool,
    pub(crate) edition: Option<String>,
    /// The errors rustc has to give for a block that is to fail, like `E0308`.
    pub(crate) error_codes: Vec<String>,
}

impl Marks {
    /// `None` for a block that is not Rust.
    pub(crate) fn parse(lang: &str) -> Option<Marks> {
        let tokens = || lang.split([',', ' ', '\t']).map(str::trim).filter(|it| !it.is_empty());
        // `ignore-<target>` marks a block as Rust the way `ignore` does.
        let plain: Vec<&str> =
            tokens().map(|it| if it.starts_with("ignore-") { "ignore" } else { it }).collect();
        if !is_rust_fence(&plain.join(",")) {
            return None;
        }
        let mut marks = Marks::default();
        for token in tokens() {
            match token {
                "ignore" => marks.ignore = true,
                "no_run" => marks.no_run = true,
                "should_panic" => marks.should_panic = true,
                "compile_fail" => marks.compile_fail = true,
                "test_harness" => marks.test_harness = true,
                _ => {
                    if let Some(on) = token.strip_prefix("ignore-") {
                        marks.ignored_on.push(on.to_owned());
                    } else if let Some(edition) = token.strip_prefix("edition") {
                        marks.edition = Some(edition.to_owned());
                    } else if is_error_code(token) {
                        marks.error_codes.push(token.to_owned());
                    }
                }
            }
        }
        Some(marks)
    }
}

fn is_error_code(token: &str) -> bool {
    token.len() == 5 && token.starts_with('E') && token[1..].parse::<u32>().is_ok()
}

/// The examples of the crate, in the order of a walk of its modules.
pub(crate) fn examples(db: &RootDatabase, krate: hir::Crate) -> Vec<Example> {
    let mut found = Found { db, examples: Vec::new(), seen: Default::default() };
    found.module(krate.root_module(db), "");
    found.examples
}

struct Found<'db> {
    db: &'db RootDatabase,
    examples: Vec<Example>,
    /// Where the blocks found are: a macro that is exported is in two modules.
    seen: rustc_hash::FxHashSet<(PathBuf, usize, String)>,
}

fn join(path: &str, name: impl std::fmt::Display) -> String {
    if path.is_empty() { name.to_string() } else { format!("{path}::{name}") }
}

impl Found<'_> {
    fn module(&mut self, module: Module, path: &str) {
        let db = self.db;
        self.documented(module, path);
        for def in module.declarations(db) {
            let named = |name: hir::Name| join(path, name.as_str());
            match def {
                ModuleDef::Module(it) => {
                    if let Some(name) = it.name(db) {
                        self.module(it, &named(name));
                    }
                }
                ModuleDef::Function(it) => self.documented(it, &named(it.name(db))),
                ModuleDef::Adt(it) => self.adt(it, &named(it.name(db))),
                ModuleDef::Const(it) => {
                    if let Some(name) = it.name(db) {
                        self.documented(it, &named(name));
                    }
                }
                ModuleDef::Static(it) => self.documented(it, &named(it.name(db))),
                ModuleDef::TypeAlias(it) => self.documented(it, &named(it.name(db))),
                ModuleDef::Trait(it) => {
                    let path = named(it.name(db));
                    self.documented(it, &path);
                    self.items(it.items(db), &path);
                }
                ModuleDef::Macro(it) => self.documented(it, &named(it.name(db))),
                _ => {}
            }
        }
        for it in module.legacy_macros(db) {
            // rustdoc has a macro at the root of the crate when it is exported.
            let path = if is_exported(db, it) { "" } else { path };
            self.documented(it, &join(path, it.name(db).as_str()));
        }
        for it in module.impl_defs(db) {
            let Some(source) = it.source(db) else { continue };
            let Some(ty) = source.value.self_ty() else { continue };
            let ty: String = ty.syntax().text().to_string().split_whitespace().collect();
            let path = join(path, ty);
            self.documented(it, &path);
            self.items(it.items(db), &path);
        }
    }

    fn items(&mut self, items: Vec<AssocItem>, path: &str) {
        for item in items {
            if let Some(name) = item.name(self.db) {
                self.documented(item, &join(path, name.as_str()));
            }
        }
    }

    fn adt(&mut self, adt: Adt, path: &str) {
        let db = self.db;
        self.documented(adt, path);
        let fields = match adt {
            Adt::Struct(it) => it.fields(db),
            Adt::Union(it) => it.fields(db),
            Adt::Enum(it) => {
                for variant in it.variants(db) {
                    let path = join(path, variant.name(db).as_str());
                    self.documented(variant, &path);
                    self.fields(variant.fields(db), &path);
                }
                Vec::new()
            }
        };
        self.fields(fields, path);
    }

    fn fields(&mut self, fields: Vec<hir::Field>, path: &str) {
        for field in fields {
            self.documented(field, &join(path, field.name(self.db).as_str()));
        }
    }

    /// Takes the examples out of the documentation of `item`, when it has any.
    fn documented(&mut self, item: impl HasAttrs, path: &str) {
        let Some(docs) = item.hir_docs(self.db) else { return };
        let text = docs.docs();
        // Where the documentation starts: a block whose place in a file is not known, one a
        // macro or `include_str!` wrote, is counted from there.
        let start = self.place(docs, TextSize::new(0));
        for block in blocks(text) {
            let Some(marks) = Marks::parse(&block.lang) else { continue };
            let offset = TextSize::new(block.start as u32);
            let lines_before = text[..block.start].matches('\n').count();
            let place = match (self.place(docs, offset), &start) {
                (Some(place), _) => place,
                (None, Some((file, line))) => (file.clone(), line + lines_before),
                (None, None) => continue,
            };
            if !self.seen.insert((place.0.clone(), place.1, path.to_owned())) {
                continue;
            }
            self.examples.push(Example {
                item: path.to_owned(),
                file: place.0,
                line: place.1,
                marks,
                code: with_hidden_lines(&block.code),
            });
        }
    }

    /// The file and line of the source that `offset` of the documentation is written at.
    fn place(&self, docs: &hir::Docs, offset: TextSize) -> Option<(PathBuf, usize)> {
        let (range, _) = docs.find_ast_range(TextRange::empty(offset))?;
        let file = range.file_id.file_id()?.file_id(self.db);
        let root = self.db.file_source_root(file).source_root_id(self.db);
        let root = self.db.source_root(root).source_root(self.db);
        let path = PathBuf::from(root.path_for_file(&file)?.as_path()?.to_string());
        let line = ide_db::line_index(self.db, file).line_col(range.value.start()).line;
        Some((path, line as usize + 1))
    }
}

fn is_exported(db: &RootDatabase, it: hir::Macro) -> bool {
    it.source(db).is_some_and(|source| {
        let attrs = source.value.syntax().children().filter_map(syntax::ast::Attr::cast);
        attrs.into_iter().any(|it| it.syntax().text().to_string().contains("macro_export"))
    })
}

/// A code block of a Markdown text.
struct Block {
    /// Where in the text it starts.
    start: usize,
    /// What follows the fence that opens it: nothing for an indented block.
    lang: String,
    code: String,
}

fn blocks(markdown: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;
    let mut blocks = Vec::new();
    let mut open: Option<Block> = None;
    for (event, range) in Parser::new_ext(markdown, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                open = Some(Block { start: range.start, lang, code: String::new() });
            }
            Event::Text(text) => {
                if let Some(block) = &mut open {
                    block.code.push_str(&text);
                }
            }
            Event::End(Tag::CodeBlock(_)) => blocks.extend(open.take()),
            _ => {}
        }
    }
    blocks
}

/// The code of a block as it is compiled: a line that starts with `# ` is not shown and is
/// code without it, and `##` is how a line that starts with `#` is written.
fn with_hidden_lines(code: &str) -> String {
    let line = |line: &str| {
        let trimmed = line.trim();
        if trimmed.starts_with("##") {
            line.replacen("##", "#", 1)
        } else if let Some(hidden) = trimmed.strip_prefix("# ") {
            hidden.to_owned()
        } else if trimmed == "#" {
            String::new()
        } else {
            line.to_owned()
        }
    };
    code.lines().map(line).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_of_a_block() {
        assert_eq!(Marks::parse(""), Some(Marks::default()));
        assert_eq!(Marks::parse("text"), None);
        assert_eq!(Marks::parse("sh,ignore"), None);
        let marks = Marks::parse("rust, should_panic,edition2018").unwrap();
        assert!(marks.should_panic && marks.edition.as_deref() == Some("2018"));
        let marks = Marks::parse("compile_fail,E0308").unwrap();
        assert!(marks.compile_fail && marks.error_codes == ["E0308"]);
        let marks = Marks::parse("ignore-wasm32").unwrap();
        assert!(!marks.ignore && marks.ignored_on == ["wasm32"]);
    }

    #[test]
    fn blocks_fenced_and_indented() {
        let found =
            blocks("Text.\n\n```no_run\nlet a = 1;\n```\n\n    let b = 2;\n\n~~~text\nc\n~~~\n");
        let found: Vec<_> = found.iter().map(|it| (it.lang.as_str(), it.code.as_str())).collect();
        assert_eq!(found, [("no_run", "let a = 1;\n"), ("", "let b = 2;\n"), ("text", "c\n")]);
    }

    #[test]
    fn hidden_lines_are_code() {
        assert_eq!(
            with_hidden_lines("# let a = 1;\n  #\n##[derive(Debug)]\n#[x]\nb"),
            "let a = 1;\n\n#[derive(Debug)]\n#[x]\nb"
        );
    }
}
