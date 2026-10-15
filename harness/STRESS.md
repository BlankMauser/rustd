# Stress edits

88 of 88 steps right, 0 restarts. Log: /tmp/rustd-stress-out-tysyb7oy/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 41 | same |
| 1 | body: in main | patch | 85 | same |
| 2 | body: generic function used from another crate | patch | 46 | same |
| 3 | body: generic method of a generic struct | patch | 39 | same |
| 4 | body: trait default method | patch | 38 | same |
| 5 | body: closure inside a function | patch | 62 | same |
| 6 | body: async fn | patch | 44 | same |
| 7 | body: async fn gets another await | patch | 85 | same |
| 8 | body: Drop impl | patch | 56 | same |
| 9 | body: match arm in trait impl | patch | 45 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 54 | same |
| 11 | blank line above a panic site | patch | 49 | same |
| 12 | comment appended | patch | 38 | same |
| 13 | new private function, unused | patch | 55 | same |
| 14 | new public function, called from main | patch | 126 | same |
| 15 | function renamed, callers updated | patch | 138 | same |
| 16 | function removed | patch | 54 | same |
| 17 | function moved to another file of the crate | patch | 88 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 105 | same |
| 19 | signature: return type changed | patch | 147 | same |
| 20 | signature: parameter type &str to String | patch | 125 | same |
| 21 | signature: function made generic | full | 113 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 88 | same |
| 23 | signature: const fn body and a const that uses it | patch | 80 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 104 | same |
| 25 | struct: field added, constructor updated | patch | 155 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 97 | same |
| 27 | struct: field type widened across crates | patch | 178 | same |
| 28 | struct: private field added to a generic struct | patch | 160 | same |
| 29 | enum: variant added, matches updated | patch | 94 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 88 | same |
| 31 | derive added | patch | 104 | same |
| 32 | derive removed, hand-written impl instead | full | 183 | same |
| 33 | repr and type alias | patch | 86 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 149 | same |
| 35 | trait: default overridden in one impl | patch | 65 | same |
| 36 | trait: required method added, all impls updated | patch | 123 | same |
| 37 | trait: associated const changed | patch | 46 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 114 | same |
| 39 | trait: impl removed | patch | 125 | same |
| 40 | trait: new trait with blanket impl | patch | 86 | same |
| 41 | trait: generic bound tightened | patch | 130 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 199 | same |
| 43 | trait: operator impl added | patch | 193 | same |
| 44 | generic: new instantiation of an existing generic | patch | 173 | same |
| 45 | generic: std collection never used before | patch | 270 | same |
| 46 | generic: type parameter added to a function | patch | 117 | same |
| 47 | generic: const generic introduced | patch | 132 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 70 | same |
| 49 | macro: function-making macro changed | patch | 76 | same |
| 50 | macro: new invocation makes a new function | patch | 125 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 161 | same |
| 52 | macro: macro that declares a struct and impl | patch | 136 | same |
| 53 | macro: format string changed | patch | 173 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 219 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 130 | same |
| 56 | static value changed | full | 157 | same |
| 57 | static added and used | patch | 186 | same |
| 58 | static initial value of an atomic changed | full | 100 | same |
| 59 | thread_local added | patch | 116 | same |
| 60 | static with a lazily made value | patch | 159 | same |
| 61 | new file and module | patch | 151 | same |
| 62 | edit to the file just added | patch | 47 | same |
| 63 | module renamed (file moved) | patch | 126 | same |
| 64 | module becomes a directory with a submodule | patch | 118 | same |
| 65 | re-export added and used | patch | 132 | same |
| 66 | inline module with nested function and impl | patch | 108 | same |
| 67 | include_str! of a new file | patch | 125 | same |
| 68 | the included file changes | patch | 69 | same |
| 69 | module removed | patch | 119 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 54 | same |
| 71 | cfg(debug_assertions) branch | patch | 117 | same |
| 72 | visibility narrowed and widened | patch | 68 | same |
| 73 | BROKEN: syntax error | error | 375 | survived (error: /tmp/rustd-stress-h3ui2d2u/mid/src/text.rs:17: expect) |
| 74 | fixed again, with a body change | full | 93 | same |
| 75 | BROKEN: type error in a body | error | 143 | survived (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 7 | survived (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 73 | same |
| 78 | BROKEN: unresolved name in main | error | 58 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 147 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 57 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 152 | same |
| 82 | BROKEN: file emptied | error | 110 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 161 | same |
| 84 | whole file rewritten in another order | patch | 68 | same |
| 85 | large generated function added | patch | 584 | same |
| 86 | everything back to the first source | full | 126 | same |
| 87 | and forward again to the last source | full | 562 | same |
