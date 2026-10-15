# Stress edits

88 of 88 steps right, 0 restarts. Log: /tmp/rustd-stress-out-cqvtc6ig/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 33 | same |
| 1 | body: in main | patch | 48 | same |
| 2 | body: generic function used from another crate | patch | 33 | same |
| 3 | body: generic method of a generic struct | patch | 35 | same |
| 4 | body: trait default method | patch | 32 | same |
| 5 | body: closure inside a function | patch | 48 | same |
| 6 | body: async fn | patch | 33 | same |
| 7 | body: async fn gets another await | patch | 49 | same |
| 8 | body: Drop impl | patch | 37 | same |
| 9 | body: match arm in trait impl | patch | 32 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 33 | same |
| 11 | blank line above a panic site | patch | 31 | same |
| 12 | comment appended | patch | 30 | same |
| 13 | new private function, unused | patch | 33 | same |
| 14 | new public function, called from main | patch | 94 | same |
| 15 | function renamed, callers updated | patch | 55 | same |
| 16 | function removed | patch | 34 | same |
| 17 | function moved to another file of the crate | patch | 66 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 60 | same |
| 19 | signature: return type changed | patch | 70 | same |
| 20 | signature: parameter type &str to String | patch | 63 | same |
| 21 | signature: function made generic | full | 76 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 55 | same |
| 23 | signature: const fn body and a const that uses it | patch | 45 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 56 | same |
| 25 | struct: field added, constructor updated | patch | 64 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 55 | same |
| 27 | struct: field type widened across crates | patch | 54 | same |
| 28 | struct: private field added to a generic struct | patch | 54 | same |
| 29 | enum: variant added, matches updated | patch | 64 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 59 | same |
| 31 | derive added | patch | 61 | same |
| 32 | derive removed, hand-written impl instead | full | 84 | same |
| 33 | repr and type alias | patch | 64 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 65 | same |
| 35 | trait: default overridden in one impl | patch | 46 | same |
| 36 | trait: required method added, all impls updated | patch | 61 | same |
| 37 | trait: associated const changed | patch | 40 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 63 | same |
| 39 | trait: impl removed | patch | 60 | same |
| 40 | trait: new trait with blanket impl | patch | 64 | same |
| 41 | trait: generic bound tightened | patch | 55 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 76 | same |
| 43 | trait: operator impl added | patch | 65 | same |
| 44 | generic: new instantiation of an existing generic | patch | 56 | same |
| 45 | generic: std collection never used before | patch | 185 | same |
| 46 | generic: type parameter added to a function | patch | 72 | same |
| 47 | generic: const generic introduced | patch | 69 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 43 | same |
| 49 | macro: function-making macro changed | patch | 48 | same |
| 50 | macro: new invocation makes a new function | patch | 91 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 84 | same |
| 52 | macro: macro that declares a struct and impl | patch | 79 | same |
| 53 | macro: format string changed | patch | 44 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 87 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 72 | same |
| 56 | static value changed | full | 77 | same |
| 57 | static added and used | patch | 64 | same |
| 58 | static initial value of an atomic changed | full | 67 | same |
| 59 | thread_local added | patch | 70 | same |
| 60 | static with a lazily made value | patch | 85 | same |
| 61 | new file and module | patch | 73 | same |
| 62 | edit to the file just added | patch | 37 | same |
| 63 | module renamed (file moved) | patch | 71 | same |
| 64 | module becomes a directory with a submodule | patch | 67 | same |
| 65 | re-export added and used | patch | 79 | same |
| 66 | inline module with nested function and impl | patch | 76 | same |
| 67 | include_str! of a new file | patch | 78 | same |
| 68 | the included file changes | patch | 58 | same |
| 69 | module removed | patch | 72 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 38 | same |
| 71 | cfg(debug_assertions) branch | patch | 81 | same |
| 72 | visibility narrowed and widened | patch | 40 | same |
| 73 | BROKEN: syntax error | error | 314 | survived (error: /tmp/rustd-stress-zbmro64t/mid/src/text.rs:17: expect) |
| 74 | fixed again, with a body change | full | 76 | same |
| 75 | BROKEN: type error in a body | error | 96 | survived (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 6 | survived (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 65 | same |
| 78 | BROKEN: unresolved name in main | error | 25 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 91 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 25 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 108 | same |
| 82 | BROKEN: file emptied | error | 23 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 102 | same |
| 84 | whole file rewritten in another order | patch | 41 | same |
| 85 | large generated function added | patch | 215 | same |
| 86 | everything back to the first source | full | 103 | same |
| 87 | and forward again to the last source | full | 244 | same |
