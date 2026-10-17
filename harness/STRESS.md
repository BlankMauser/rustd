# Stress edits

99 of 99 steps right, 0 restarts. Log: /tmp/rustd-stress-out-demxn915/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 45 | same |
| 1 | body: in main | patch | 71 | same |
| 2 | body: generic function used from another crate | patch | 32 | same |
| 3 | body: generic method of a generic struct | patch | 30 | same |
| 4 | body: trait default method | patch | 31 | same |
| 5 | body: closure inside a function | patch | 37 | same |
| 6 | body: async fn | patch | 81 | same |
| 7 | body: async fn gets another await | patch | 80 | same |
| 8 | body: Drop impl | patch | 65 | same |
| 9 | body: match arm in trait impl | patch | 29 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 23 | same |
| 11 | blank line above a panic site | patch | 35 | same |
| 12 | comment appended | patch | 27 | same |
| 13 | new private function, unused | patch | 23 | same |
| 14 | new public function, called from main | patch | 124 | same |
| 15 | function renamed, callers updated | patch | 88 | same |
| 16 | function removed | patch | 37 | same |
| 17 | function moved to another file of the crate | patch | 80 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 80 | same |
| 19 | signature: return type changed | patch | 111 | same |
| 20 | signature: parameter type &str to String | patch | 73 | same |
| 21 | signature: function made generic | full | 155 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 91 | same |
| 23 | signature: const fn body and a const that uses it | patch | 125 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 80 | same |
| 25 | struct: field added, constructor updated | patch | 69 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 75 | same |
| 27 | struct: field type widened across crates | patch | 89 | same |
| 28 | struct: private field added to a generic struct | patch | 70 | same |
| 29 | enum: variant added, matches updated | patch | 105 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 108 | same |
| 31 | derive added | patch | 72 | same |
| 32 | derive removed, hand-written impl instead | full | 240 | same |
| 33 | repr and type alias | patch | 93 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 134 | same |
| 35 | trait: default overridden in one impl | patch | 71 | same |
| 36 | trait: required method added, all impls updated | patch | 113 | same |
| 37 | trait: associated const changed | patch | 28 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 101 | same |
| 39 | trait: impl removed | patch | 83 | same |
| 40 | trait: new trait with blanket impl | patch | 83 | same |
| 41 | trait: generic bound tightened | patch | 97 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 117 | same |
| 43 | trait: operator impl added | patch | 143 | same |
| 44 | generic: new instantiation of an existing generic | patch | 86 | same |
| 45 | generic: std collection never used before | patch | 427 | same |
| 46 | generic: type parameter added to a function | patch | 87 | same |
| 47 | generic: const generic introduced | patch | 124 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 100 | same |
| 49 | macro: function-making macro changed | patch | 48 | same |
| 50 | macro: new invocation makes a new function | patch | 114 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 111 | same |
| 52 | macro: macro that declares a struct and impl | patch | 136 | same |
| 53 | macro: format string changed | patch | 70 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 153 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 96 | same |
| 56 | static value changed | patch | 108 | same |
| 57 | static added and used | patch | 121 | same |
| 58 | static initial value of an atomic changed | patch | 71 | same |
| 59 | thread_local added | patch | 129 | same |
| 60 | static with a lazily made value | patch | 199 | same |
| 61 | new file and module | patch | 176 | same |
| 62 | edit to the file just added | patch | 99 | same |
| 63 | module renamed (file moved) | patch | 145 | same |
| 64 | module becomes a directory with a submodule | patch | 121 | same |
| 65 | re-export added and used | patch | 177 | same |
| 66 | inline module with nested function and impl | patch | 177 | same |
| 67 | include_str! of a new file | patch | 218 | same |
| 68 | the included file changes | patch | 119 | same |
| 69 | module removed | patch | 137 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 118 | same |
| 71 | cfg(debug_assertions) branch | patch | 252 | same |
| 72 | visibility narrowed and widened | patch | 83 | same |
| 73 | BROKEN: syntax error | error | 1992 | said so (error: /tmp/rustd-stress-nbgg2q6w/mid/src/text.rs:17: expect) |
| 74 | fixed again, with a body change | full | 145 | same |
| 75 | BROKEN: type error in a body | error | 214 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 45 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 215 | same |
| 78 | BROKEN: unresolved name in main | error | 75 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 173 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 44 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 186 | same |
| 82 | BROKEN: file emptied | error | 61 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 291 | same |
| 84 | thread_local of the first source given another value | full | 179 | same |
| 85 | BROKEN: struct literal without one of its fields | error | 25 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 86 | mended: the field given | patch | 20 | same |
| 87 | BROKEN: match without one of its arms | error | 23 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 88 | mended: the arm given | patch | 21 | same |
| 89 | BROKEN: impl without one of its trait's methods | error | 33 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 90 | mended: the method given | patch | 22 | same |
| 91 | BROKEN: method that no type has | error | 47 | said so (error: `text` has errors, `cargo check` shows them: no such ) |
| 92 | mended: the method named right | patch | 29 | same |
| 93 | BROKEN: import of a name that is not there | error | 95 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 94 | mended: the import removed | patch | 115 | same |
| 95 | whole file rewritten in another order | patch | 43 | same |
| 96 | large generated function added | patch | 219 | same |
| 97 | everything back to the first source | full | 320 | same |
| 98 | and forward again to the last source | full | 364 | same |
