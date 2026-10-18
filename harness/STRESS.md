# Stress edits

99 of 99 steps right, 0 restarts. Log: /dev/shm/rustd-stress-out-9jvtt_jy/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 9 | same |
| 1 | body: in main | patch | 45 | same |
| 2 | body: generic function used from another crate | patch | 13 | same |
| 3 | body: generic method of a generic struct | patch | 12 | same |
| 4 | body: trait default method | patch | 17 | same |
| 5 | body: closure inside a function | patch | 19 | same |
| 6 | body: async fn | patch | 15 | same |
| 7 | body: async fn gets another await | patch | 25 | same |
| 8 | body: Drop impl | patch | 14 | same |
| 9 | body: match arm in trait impl | patch | 12 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 10 | same |
| 11 | blank line above a panic site | patch | 11 | same |
| 12 | comment appended | patch | 10 | same |
| 13 | new private function, unused | patch | 11 | same |
| 14 | new public function, called from main | patch | 291 | same |
| 15 | function renamed, callers updated | patch | 101 | same |
| 16 | function removed | patch | 74 | same |
| 17 | function moved to another file of the crate | patch | 104 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 107 | same |
| 19 | signature: return type changed | patch | 101 | same |
| 20 | signature: parameter type &str to String | patch | 105 | same |
| 21 | signature: function made generic | full | 225 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 45 | same |
| 23 | signature: const fn body and a const that uses it | patch | 33 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 48 | same |
| 25 | struct: field added, constructor updated | patch | 65 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 50 | same |
| 27 | struct: field type widened across crates | patch | 47 | same |
| 28 | struct: private field added to a generic struct | patch | 51 | same |
| 29 | enum: variant added, matches updated | patch | 53 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 46 | same |
| 31 | derive added | patch | 51 | same |
| 32 | derive removed, hand-written impl instead | full | 231 | same |
| 33 | repr and type alias | patch | 54 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 79 | same |
| 35 | trait: default overridden in one impl | patch | 38 | same |
| 36 | trait: required method added, all impls updated | patch | 49 | same |
| 37 | trait: associated const changed | patch | 19 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 142 | same |
| 39 | trait: impl removed | patch | 136 | same |
| 40 | trait: new trait with blanket impl | patch | 95 | same |
| 41 | trait: generic bound tightened | patch | 90 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 85 | same |
| 43 | trait: operator impl added | patch | 52 | same |
| 44 | generic: new instantiation of an existing generic | patch | 55 | same |
| 45 | generic: std collection never used before | patch | 300 | same |
| 46 | generic: type parameter added to a function | patch | 133 | same |
| 47 | generic: const generic introduced | patch | 134 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 99 | same |
| 49 | macro: function-making macro changed | patch | 98 | same |
| 50 | macro: new invocation makes a new function | patch | 132 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 172 | same |
| 52 | macro: macro that declares a struct and impl | patch | 135 | same |
| 53 | macro: format string changed | patch | 99 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 146 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 131 | same |
| 56 | static value changed | patch | 123 | same |
| 57 | static added and used | patch | 136 | same |
| 58 | static initial value of an atomic changed | patch | 98 | same |
| 59 | thread_local added | patch | 166 | same |
| 60 | static with a lazily made value | patch | 233 | same |
| 61 | new file and module | patch | 165 | same |
| 62 | edit to the file just added | patch | 126 | same |
| 63 | module renamed (file moved) | patch | 166 | same |
| 64 | module becomes a directory with a submodule | patch | 166 | same |
| 65 | re-export added and used | patch | 169 | same |
| 66 | inline module with nested function and impl | patch | 167 | same |
| 67 | include_str! of a new file | patch | 190 | same |
| 68 | the included file changes | patch | 155 | same |
| 69 | module removed | patch | 179 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 129 | same |
| 71 | cfg(debug_assertions) branch | patch | 187 | same |
| 72 | visibility narrowed and widened | patch | 151 | same |
| 73 | BROKEN: syntax error | error | 569 | said so (error: /dev/shm/rustd-stress-ayl6stu5/mid/src/text.rs:17: ex) |
| 74 | fixed again, with a body change | full | 187 | same |
| 75 | BROKEN: type error in a body | error | 250 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 11 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 177 | same |
| 78 | BROKEN: unresolved name in main | error | 99 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 218 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 98 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 219 | same |
| 82 | BROKEN: file emptied | error | 98 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 236 | same |
| 84 | thread_local of the first source given another value | full | 178 | same |
| 85 | BROKEN: struct literal without one of its fields | error | 12 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 86 | mended: the field given | patch | 9 | same |
| 87 | BROKEN: match without one of its arms | error | 33 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 88 | mended: the arm given | patch | 12 | same |
| 89 | BROKEN: impl without one of its trait's methods | error | 26 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 90 | mended: the method given | patch | 13 | same |
| 91 | BROKEN: method that no type has | error | 10 | said so (error: `text` has errors, `cargo check` shows them: no such ) |
| 92 | mended: the method named right | patch | 8 | same |
| 93 | BROKEN: import of a name that is not there | error | 54 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 94 | mended: the import removed | patch | 55 | same |
| 95 | whole file rewritten in another order | patch | 11 | same |
| 96 | large generated function added | patch | 203 | same |
| 97 | everything back to the first source | full | 220 | same |
| 98 | and forward again to the last source | full | 493 | same |
