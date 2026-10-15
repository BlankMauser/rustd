//! Telling an edit to the bodies of functions from one the rest of the program can see.
//!
//! Most edits change what a function does and nothing else. What was compiled from other
//! bodies is then as it was, and only what comes from the edited file needs another look. That
//! holds for a body nothing else sees into: not that of a `const fn`, which other items are
//! evaluated with, nor one whose type is part of its signature (`async fn`, `-> impl Trait`),
//! nor one a macro is handed, nor one that declares items.

use std::hash::{Hash, Hasher};

use hir_def::{FunctionId, expr_store::Body};
use ide_db::{
    RootDatabase,
    base_db::{SourceDatabase as _, relevant_crates},
};
use rustc_hash::{FxHashMap, FxHasher};
use syntax::{
    AstNode, Edition, SourceFile, SyntaxKind, SyntaxNode, WalkEvent,
    ast::{self, HasAttrs},
};
use vfs::FileId;

use crate::Listed;

/// Attributes that leave the item they are on as it is written.
const INERT: &[&str] = &[
    "allow",
    "cfg",
    "cold",
    "deny",
    "deprecated",
    "doc",
    "expect",
    "forbid",
    "inline",
    "must_use",
    "track_caller",
    "warn",
];

/// Each file as it was when last looked at.
#[derive(Default)]
pub(crate) struct Outlines {
    files: FxHashMap<FileId, Option<u64>>,
}

impl Outlines {
    /// Takes note of a file that is about to change, unless it is known already.
    pub(crate) fn before_edit(&mut self, db: &RootDatabase, listed: &Listed, file: FileId) {
        self.files.entry(file).or_insert_with(|| outline(db, listed, file));
    }

    /// For each file of the edit that was just applied, whether nothing changed in it but
    /// bodies that nothing else sees into. Asked after [`Outlines::before_edit`].
    pub(crate) fn after_edit(
        &mut self,
        db: &RootDatabase,
        listed: &Listed,
        files: Vec<FileId>,
    ) -> Vec<(FileId, bool)> {
        let off = std::env::var_os("RUSTD_NO_BODIES").is_some();
        let files: Vec<_> = files
            .into_iter()
            .map(|file| {
                let now = outline(db, listed, file);
                let was = self.files.insert(file, now);
                (file, !off && now.is_some() && was == Some(now))
            })
            .collect();
        // An edit to more than bodies can change which functions there are, in any file, and
        // what is kept here was found by asking the ones listed before it.
        if files.iter().any(|&(_, bodies_only)| !bodies_only) {
            self.files.clear();
        }
        files
    }
}

/// A hash of everything in the file but the bodies nothing else sees into, or `None` when one
/// of its bodies declares items or the file is not all ours to compile.
fn outline(db: &RootDatabase, listed: &Listed, file: FileId) -> Option<u64> {
    let functions = listed.in_file(db, file)?;
    let text = db.file_text(file).text(db);
    let hash = hash_outside_bodies(&SourceFile::parse(text, Edition::CURRENT).syntax_node());
    // A function the edit took away is an edit to more than bodies, and asking for its body
    // panics.
    let declare = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::lower(db, functions.iter().map(|&it| it.into()).collect());
        functions.iter().any(|&it| declares_items(db, it))
    }));
    matches!(declare, Ok(false)).then_some(hash)
}

fn declares_items(db: &RootDatabase, func: FunctionId) -> bool {
    Body::of(db, func.into()).blocks(db).any(|(_, def_map)| {
        def_map.modules().any(|(_, module)| {
            let scope = module.scope(db);
            scope.declarations().next().is_some()
                || scope.impls().len() != 0
                || scope.unnamed_consts().next().is_some()
        })
    })
}

fn hash_outside_bodies(file: &SyntaxNode) -> u64 {
    let mut hasher = FxHasher::default();
    let mut walk = file.preorder_with_tokens();
    while let Some(event) = walk.next() {
        match event {
            WalkEvent::Enter(syntax::NodeOrToken::Node(node)) => {
                node.kind().hash(&mut hasher);
                if is_unseen_body(&node) {
                    walk.skip_subtree();
                }
            }
            WalkEvent::Enter(syntax::NodeOrToken::Token(token)) => {
                // Blank space says nothing, except to a macro that is handed it.
                let blank = token.kind() == SyntaxKind::WHITESPACE
                    && token.parent().is_none_or(|it| it.kind() != SyntaxKind::TOKEN_TREE);
                if !blank {
                    token.kind().hash(&mut hasher);
                    token.text().hash(&mut hasher);
                }
            }
            WalkEvent::Leave(_) => {}
        }
    }
    hasher.finish()
}

/// Whether the node is the body of a function that nothing else sees into, going by how the
/// function is written. What its body declares takes name resolution to tell.
fn is_unseen_body(node: &SyntaxNode) -> bool {
    if node.kind() != SyntaxKind::BLOCK_EXPR {
        return false;
    }
    let Some(func) = node.parent().and_then(ast::Fn::cast) else { return false };
    if func.const_token().is_some() || func.async_token().is_some() || func.gen_token().is_some() {
        return false;
    }
    let names_its_type = func.ret_type().is_some_and(|it| {
        it.syntax().descendants().any(|it| it.kind() == SyntaxKind::IMPL_TRAIT_TYPE)
    });
    if names_its_type || !is_inert(func.syntax()) {
        return false;
    }
    // An attribute written inside the body is the function's, and a macro declared in it can
    // be exported from the crate.
    let reaches_out = node.descendants().any(|it| match it.kind() {
        SyntaxKind::ATTR => it.parent().is_some_and(|parent| parent == *node),
        SyntaxKind::MACRO_RULES | SyntaxKind::MACRO_DEF => true,
        _ => false,
    });
    if reaches_out {
        return false;
    }
    // Only items, all the way up: a function in a body is that body's, and one in a macro's
    // input is the macro's.
    func.syntax().ancestors().skip(1).all(|it| match it.kind() {
        SyntaxKind::SOURCE_FILE | SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST => true,
        SyntaxKind::MODULE | SyntaxKind::IMPL | SyntaxKind::TRAIT => is_inert(&it),
        _ => false,
    })
}

/// Whether no attribute on the item can be a macro that rewrites it.
fn is_inert(item: &SyntaxNode) -> bool {
    let Some(item) = ast::AnyHasAttrs::cast(item.clone()) else { return false };
    item.attrs().all(|attr| attr.simple_name().is_some_and(|name| INERT.contains(&name.as_str())))
}

impl Listed {
    /// The functions written in the file, or `None` when it has some that are not listed: it
    /// belongs to a crate that the ones being built use and that is not one of them.
    fn in_file(&self, db: &RootDatabase, file: FileId) -> Option<Vec<FunctionId>> {
        let mut functions = Vec::new();
        for &krate in relevant_crates(db, file).iter() {
            match self.functions.get(&krate.into()) {
                Some(listed) => functions.extend(
                    listed
                        .iter()
                        .copied()
                        .filter(|&it| hir_ty::mir::codegen::body_file(db, it.into()) == file),
                ),
                None if self.functions.keys().any(|&built| {
                    ide_db::base_db::Crate::from(built).transitive_deps(db).contains(&krate)
                }) =>
                {
                    return None;
                }
                // Nothing that is built can see it.
                None => {}
            }
        }
        Some(functions)
    }
}
