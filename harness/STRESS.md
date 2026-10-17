# Stress edits

99 of 99 steps right, 0 restarts. Log: /tmp/rustd-stress-out-oyy6qc5i/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 22 | same |
| 1 | body: in main | patch | 65 | same |
| 2 | body: generic function used from another crate | patch | 26 | same |
| 3 | body: generic method of a generic struct | patch | 30 | same |
| 4 | body: trait default method | patch | 24 | same |
| 5 | body: closure inside a function | patch | 54 | same |
| 6 | body: async fn | patch | 29 | same |
| 7 | body: async fn gets another await | patch | 53 | same |
| 8 | body: Drop impl | patch | 27 | same |
| 9 | body: match arm in trait impl | patch | 24 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 21 | same |
| 11 | blank line above a panic site | patch | 23 | same |
| 12 | comment appended | patch | 22 | same |
| 13 | new private function, unused | patch | 23 | same |
| 14 | new public function, called from main | patch | 118 | same |
| 15 | function renamed, callers updated | patch | 70 | same |
| 16 | function removed | patch | 24 | same |
| 17 | function moved to another file of the crate | patch | 71 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 92 | same |
| 19 | signature: return type changed | patch | 83 | same |
| 20 | signature: parameter type &str to String | patch | 66 | same |
| 21 | signature: function made generic | full | 120 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 70 | same |
| 23 | signature: const fn body and a const that uses it | patch | 55 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 74 | same |
| 25 | struct: field added, constructor updated | patch | 70 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 70 | same |
| 27 | struct: field type widened across crates | patch | 67 | same |
| 28 | struct: private field added to a generic struct | patch | 67 | same |
| 29 | enum: variant added, matches updated | patch | 89 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 68 | same |
| 31 | derive added | patch | 78 | same |
| 32 | derive removed, hand-written impl instead | full | 149 | same |
| 33 | repr and type alias | patch | 81 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 73 | same |
| 35 | trait: default overridden in one impl | patch | 64 | same |
| 36 | trait: required method added, all impls updated | patch | 81 | same |
| 37 | trait: associated const changed | patch | 28 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 85 | same |
| 39 | trait: impl removed | patch | 73 | same |
| 40 | trait: new trait with blanket impl | patch | 80 | same |
| 41 | trait: generic bound tightened | patch | 82 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 106 | same |
| 43 | trait: operator impl added | patch | 82 | same |
| 44 | generic: new instantiation of an existing generic | patch | 84 | same |
| 45 | generic: std collection never used before | patch | 284 | same |
| 46 | generic: type parameter added to a function | patch | 90 | same |
| 47 | generic: const generic introduced | patch | 91 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 38 | same |
| 49 | macro: function-making macro changed | patch | 38 | same |
| 50 | macro: new invocation makes a new function | patch | 95 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 105 | same |
| 52 | macro: macro that declares a struct and impl | patch | 111 | same |
| 53 | macro: format string changed | patch | 53 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 122 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 90 | same |
| 56 | static value changed | patch | 81 | same |
| 57 | static added and used | patch | 96 | same |
| 58 | static initial value of an atomic changed | patch | 40 | same |
| 59 | thread_local added | patch | 132 | same |
| 60 | static with a lazily made value | patch | 154 | same |
| 61 | new file and module | patch | 119 | same |
| 62 | edit to the file just added | patch | 60 | same |
| 63 | module renamed (file moved) | patch | 118 | same |
| 64 | module becomes a directory with a submodule | patch | 119 | same |
| 65 | re-export added and used | patch | 117 | same |
| 66 | inline module with nested function and impl | patch | 121 | same |
| 67 | include_str! of a new file | patch | 153 | same |
| 68 | the included file changes | patch | 125 | same |
| 69 | module removed | patch | 132 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 67 | same |
| 71 | cfg(debug_assertions) branch | patch | 172 | same |
| 72 | visibility narrowed and widened | patch | 64 | same |
| 73 | BROKEN: syntax error | error | 1795 | said so (error: /tmp/rustd-stress-9m2wjjux/mid/src/text.rs:17: expect) |
| 74 | fixed again, with a body change | full | 130 | same |
| 75 | BROKEN: type error in a body | error | 131 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 22 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 93 | same |
| 78 | BROKEN: unresolved name in main | error | 40 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 142 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 40 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 158 | same |
| 82 | BROKEN: file emptied | error | 42 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 177 | same |
| 84 | thread_local of the first source given another value | full | 81 | same |
| 85 | BROKEN: struct literal without one of its fields | error | 23 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 86 | mended: the field given | patch | 20 | same |
| 87 | BROKEN: match without one of its arms | error | 22 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 88 | mended: the arm given | patch | 21 | same |
| 89 | BROKEN: impl without one of its trait's methods | error | 33 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 90 | mended: the method given | patch | 22 | same |
| 91 | BROKEN: method that no type has | error | 20 | said so (error: `text` has errors, `cargo check` shows them: no such ) |
| 92 | mended: the method named right | patch | 22 | same |
| 93 | BROKEN: import of a name that is not there | error | 95 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 94 | mended: the import removed | patch | 90 | same |
| 95 | whole file rewritten in another order | patch | 21 | same |
| 96 | large generated function added | patch | 205 | same |
| 97 | everything back to the first source | full | 183 | same |
| 98 | and forward again to the last source | full | 219 | same |
