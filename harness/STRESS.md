# Stress edits

99 of 99 steps right, 0 restarts. Log: /tmp/rustd-stress-out-y117hmkp/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 6 | same |
| 1 | body: in main | patch | 26 | same |
| 2 | body: generic function used from another crate | patch | 8 | same |
| 3 | body: generic method of a generic struct | patch | 18 | same |
| 4 | body: trait default method | patch | 9 | same |
| 5 | body: closure inside a function | patch | 17 | same |
| 6 | body: async fn | patch | 9 | same |
| 7 | body: async fn gets another await | patch | 20 | same |
| 8 | body: Drop impl | patch | 10 | same |
| 9 | body: match arm in trait impl | patch | 9 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 8 | same |
| 11 | blank line above a panic site | patch | 8 | same |
| 12 | comment appended | patch | 7 | same |
| 13 | new private function, unused | patch | 8 | same |
| 14 | new public function, called from main | patch | 65 | same |
| 15 | function renamed, callers updated | patch | 33 | same |
| 16 | function removed | patch | 14 | same |
| 17 | function moved to another file of the crate | patch | 34 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 42 | same |
| 19 | signature: return type changed | patch | 32 | same |
| 20 | signature: parameter type &str to String | patch | 34 | same |
| 21 | signature: function made generic | full | 91 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 35 | same |
| 23 | signature: const fn body and a const that uses it | patch | 22 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 35 | same |
| 25 | struct: field added, constructor updated | patch | 34 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 38 | same |
| 27 | struct: field type widened across crates | patch | 35 | same |
| 28 | struct: private field added to a generic struct | patch | 33 | same |
| 29 | enum: variant added, matches updated | patch | 43 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 36 | same |
| 31 | derive added | patch | 36 | same |
| 32 | derive removed, hand-written impl instead | full | 91 | same |
| 33 | repr and type alias | patch | 36 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 35 | same |
| 35 | trait: default overridden in one impl | patch | 25 | same |
| 36 | trait: required method added, all impls updated | patch | 39 | same |
| 37 | trait: associated const changed | patch | 13 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 45 | same |
| 39 | trait: impl removed | patch | 40 | same |
| 40 | trait: new trait with blanket impl | patch | 39 | same |
| 41 | trait: generic bound tightened | patch | 40 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 60 | same |
| 43 | trait: operator impl added | patch | 38 | same |
| 44 | generic: new instantiation of an existing generic | patch | 42 | same |
| 45 | generic: std collection never used before | patch | 130 | same |
| 46 | generic: type parameter added to a function | patch | 53 | same |
| 47 | generic: const generic introduced | patch | 52 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 31 | same |
| 49 | macro: function-making macro changed | patch | 27 | same |
| 50 | macro: new invocation makes a new function | patch | 57 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 56 | same |
| 52 | macro: macro that declares a struct and impl | patch | 54 | same |
| 53 | macro: format string changed | patch | 27 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 66 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 50 | same |
| 56 | static value changed | patch | 53 | same |
| 57 | static added and used | patch | 83 | same |
| 58 | static initial value of an atomic changed | patch | 33 | same |
| 59 | thread_local added | patch | 84 | same |
| 60 | static with a lazily made value | patch | 102 | same |
| 61 | new file and module | patch | 80 | same |
| 62 | edit to the file just added | patch | 47 | same |
| 63 | module renamed (file moved) | patch | 86 | same |
| 64 | module becomes a directory with a submodule | patch | 90 | same |
| 65 | re-export added and used | patch | 83 | same |
| 66 | inline module with nested function and impl | patch | 82 | same |
| 67 | include_str! of a new file | patch | 93 | same |
| 68 | the included file changes | patch | 73 | same |
| 69 | module removed | patch | 84 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 58 | same |
| 71 | cfg(debug_assertions) branch | patch | 90 | same |
| 72 | visibility narrowed and widened | patch | 51 | same |
| 73 | BROKEN: syntax error | error | 352 | said so (error: /tmp/rustd-stress-w8yl0epl/mid/src/text.rs:17: expect) |
| 74 | fixed again, with a body change | full | 84 | same |
| 75 | BROKEN: type error in a body | error | 113 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 7 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 78 | same |
| 78 | BROKEN: unresolved name in main | error | 26 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 96 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 26 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 117 | same |
| 82 | BROKEN: file emptied | error | 26 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 103 | same |
| 84 | thread_local of the first source given another value | full | 74 | same |
| 85 | BROKEN: struct literal without one of its fields | error | 9 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 86 | mended: the field given | patch | 7 | same |
| 87 | BROKEN: match without one of its arms | error | 9 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 88 | mended: the arm given | patch | 8 | same |
| 89 | BROKEN: impl without one of its trait's methods | error | 19 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 90 | mended: the method given | patch | 8 | same |
| 91 | BROKEN: method that no type has | error | 7 | said so (error: `text` has errors, `cargo check` shows them: no such ) |
| 92 | mended: the method named right | patch | 6 | same |
| 93 | BROKEN: import of a name that is not there | error | 40 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 94 | mended: the import removed | patch | 37 | same |
| 95 | whole file rewritten in another order | patch | 8 | same |
| 96 | large generated function added | patch | 144 | same |
| 97 | everything back to the first source | full | 105 | same |
| 98 | and forward again to the last source | full | 202 | same |
