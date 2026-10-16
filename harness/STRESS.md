# Stress edits

99 of 99 steps right, 0 restarts. Log: /var/folders/4j/y2_1g_f91y16shdcd__42kfr0000gn/T/rustd-stress-out-1nkh_a0m/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 40 | same |
| 1 | body: in main | patch | 53 | same |
| 2 | body: generic function used from another crate | patch | 44 | same |
| 3 | body: generic method of a generic struct | patch | 44 | same |
| 4 | body: trait default method | patch | 42 | same |
| 5 | body: closure inside a function | patch | 42 | same |
| 6 | body: async fn | patch | 41 | same |
| 7 | body: async fn gets another await | patch | 50 | same |
| 8 | body: Drop impl | patch | 42 | same |
| 9 | body: match arm in trait impl | patch | 38 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 42 | same |
| 11 | blank line above a panic site | patch | 45 | same |
| 12 | comment appended | patch | 43 | same |
| 13 | new private function, unused | patch | 41 | same |
| 14 | new public function, called from main | patch | 66 | same |
| 15 | function renamed, callers updated | patch | 66 | same |
| 16 | function removed | patch | 45 | same |
| 17 | function moved to another file of the crate | patch | 58 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 58 | same |
| 19 | signature: return type changed | patch | 56 | same |
| 20 | signature: parameter type &str to String | patch | 60 | same |
| 21 | signature: function made generic | full | 67 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 53 | same |
| 23 | signature: const fn body and a const that uses it | patch | 51 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 56 | same |
| 25 | struct: field added, constructor updated | patch | 56 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 52 | same |
| 27 | struct: field type widened across crates | patch | 56 | same |
| 28 | struct: private field added to a generic struct | patch | 54 | same |
| 29 | enum: variant added, matches updated | patch | 57 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 58 | same |
| 31 | derive added | patch | 58 | same |
| 32 | derive removed, hand-written impl instead | full | 70 | same |
| 33 | repr and type alias | patch | 59 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 55 | same |
| 35 | trait: default overridden in one impl | patch | 52 | same |
| 36 | trait: required method added, all impls updated | patch | 60 | same |
| 37 | trait: associated const changed | patch | 40 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 60 | same |
| 39 | trait: impl removed | patch | 57 | same |
| 40 | trait: new trait with blanket impl | patch | 59 | same |
| 41 | trait: generic bound tightened | patch | 56 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 64 | same |
| 43 | trait: operator impl added | patch | 55 | same |
| 44 | generic: new instantiation of an existing generic | patch | 55 | same |
| 45 | generic: std collection never used before | patch | 115 | same |
| 46 | generic: type parameter added to a function | patch | 65 | same |
| 47 | generic: const generic introduced | patch | 65 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 47 | same |
| 49 | macro: function-making macro changed | patch | 47 | same |
| 50 | macro: new invocation makes a new function | patch | 64 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 73 | same |
| 52 | macro: macro that declares a struct and impl | patch | 68 | same |
| 53 | macro: format string changed | patch | 46 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 70 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 65 | same |
| 56 | static value changed | patch | 60 | same |
| 57 | static added and used | patch | 74 | same |
| 58 | static initial value of an atomic changed | patch | 52 | same |
| 59 | thread_local added | patch | 67 | same |
| 60 | static with a lazily made value | patch | 76 | same |
| 61 | new file and module | patch | 71 | same |
| 62 | edit to the file just added | patch | 50 | same |
| 63 | module renamed (file moved) | patch | 64 | same |
| 64 | module becomes a directory with a submodule | patch | 68 | same |
| 65 | re-export added and used | patch | 66 | same |
| 66 | inline module with nested function and impl | patch | 66 | same |
| 67 | include_str! of a new file | patch | 73 | same |
| 68 | the included file changes | patch | 61 | same |
| 69 | module removed | patch | 68 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 51 | same |
| 71 | cfg(debug_assertions) branch | patch | 75 | same |
| 72 | visibility narrowed and widened | patch | 48 | same |
| 73 | BROKEN: syntax error | error | 155 | said so (error: /var/folders/4j/y2_1g_f91y16shdcd__42kfr0000gn/T/rust) |
| 74 | fixed again, with a body change | full | 69 | same |
| 75 | BROKEN: type error in a body | error | 62 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 10 | said so (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 62 | same |
| 78 | BROKEN: unresolved name in main | error | 17 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 80 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 17 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 76 | same |
| 82 | BROKEN: file emptied | error | 17 | said so (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 79 | same |
| 84 | thread_local of the first source given another value | full | 60 | same |
| 85 | BROKEN: struct literal without one of its fields | error | 13 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 86 | mended: the field given | patch | 44 | same |
| 87 | BROKEN: match without one of its arms | error | 12 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 88 | mended: the arm given | patch | 44 | same |
| 89 | BROKEN: impl without one of its trait's methods | error | 13 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 90 | mended: the method given | patch | 44 | same |
| 91 | BROKEN: method that no type has | error | 10 | said so (error: `text` has errors, `cargo check` shows them: no such ) |
| 92 | mended: the method named right | patch | 39 | same |
| 93 | BROKEN: import of a name that is not there | error | 27 | said so (error: the crate root has errors, `cargo check` shows them: ) |
| 94 | mended: the import removed | patch | 60 | same |
| 95 | whole file rewritten in another order | patch | 43 | same |
| 96 | large generated function added | patch | 149 | same |
| 97 | everything back to the first source | full | 78 | same |
| 98 | and forward again to the last source | full | 164 | same |
