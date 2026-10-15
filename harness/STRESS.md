# Stress edits

88 of 88 steps right, 0 restarts. Log: /tmp/rustd-stress-out-46zmke5r/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 69 | same |
| 1 | body: in main | patch | 106 | same |
| 2 | body: generic function used from another crate | patch | 56 | same |
| 3 | body: generic method of a generic struct | patch | 44 | same |
| 4 | body: trait default method | patch | 58 | same |
| 5 | body: closure inside a function | patch | 66 | same |
| 6 | body: async fn | patch | 51 | same |
| 7 | body: async fn gets another await | patch | 68 | same |
| 8 | body: Drop impl | patch | 51 | same |
| 9 | body: match arm in trait impl | patch | 46 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 48 | same |
| 11 | blank line above a panic site | patch | 51 | same |
| 12 | comment appended | patch | 46 | same |
| 13 | new private function, unused | patch | 52 | same |
| 14 | new public function, called from main | patch | 139 | same |
| 15 | function renamed, callers updated | patch | 145 | same |
| 16 | function removed | patch | 56 | same |
| 17 | function moved to another file of the crate | patch | 96 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 105 | same |
| 19 | signature: return type changed | patch | 135 | same |
| 20 | signature: parameter type &str to String | patch | 110 | same |
| 21 | signature: function made generic | full | 136 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 113 | same |
| 23 | signature: const fn body and a const that uses it | patch | 64 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 132 | same |
| 25 | struct: field added, constructor updated | patch | 113 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 109 | same |
| 27 | struct: field type widened across crates | patch | 92 | same |
| 28 | struct: private field added to a generic struct | patch | 83 | same |
| 29 | enum: variant added, matches updated | patch | 121 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 96 | same |
| 31 | derive added | patch | 125 | same |
| 32 | derive removed, hand-written impl instead | full | 132 | same |
| 33 | repr and type alias | patch | 105 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 134 | same |
| 35 | trait: default overridden in one impl | patch | 80 | same |
| 36 | trait: required method added, all impls updated | patch | 150 | same |
| 37 | trait: associated const changed | patch | 58 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 153 | same |
| 39 | trait: impl removed | patch | 105 | same |
| 40 | trait: new trait with blanket impl | patch | 115 | same |
| 41 | trait: generic bound tightened | patch | 94 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 135 | same |
| 43 | trait: operator impl added | patch | 107 | same |
| 44 | generic: new instantiation of an existing generic | patch | 145 | same |
| 45 | generic: std collection never used before | patch | 326 | same |
| 46 | generic: type parameter added to a function | patch | 177 | same |
| 47 | generic: const generic introduced | patch | 128 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 61 | same |
| 49 | macro: function-making macro changed | patch | 81 | same |
| 50 | macro: new invocation makes a new function | patch | 159 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 153 | same |
| 52 | macro: macro that declares a struct and impl | patch | 158 | same |
| 53 | macro: format string changed | patch | 79 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 152 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 205 | same |
| 56 | static value changed | full | 160 | same |
| 57 | static added and used | patch | 171 | same |
| 58 | static initial value of an atomic changed | full | 100 | same |
| 59 | thread_local added | patch | 149 | same |
| 60 | static with a lazily made value | patch | 185 | same |
| 61 | new file and module | patch | 131 | same |
| 62 | edit to the file just added | patch | 48 | same |
| 63 | module renamed (file moved) | patch | 155 | same |
| 64 | module becomes a directory with a submodule | patch | 126 | same |
| 65 | re-export added and used | patch | 183 | same |
| 66 | inline module with nested function and impl | patch | 154 | same |
| 67 | include_str! of a new file | patch | 179 | same |
| 68 | the included file changes | patch | 89 | same |
| 69 | module removed | patch | 171 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 68 | same |
| 71 | cfg(debug_assertions) branch | patch | 166 | same |
| 72 | visibility narrowed and widened | patch | 67 | same |
| 73 | BROKEN: syntax error | error | 723 | survived (error: /tmp/rustd-stress-jpmwt26v/mid/src/text.rs:17: expect) |
| 74 | fixed again, with a body change | full | 382 | same |
| 75 | BROKEN: type error in a body | error | 183 | survived (error: `main` is not compiled by rustd and rustc's copy is f) |
| 76 | BROKEN: still broken, another file edited too | error | 12 | survived (error: `main` is not compiled by rustd and rustc's copy is f) |
| 77 | fixed again | full | 90 | same |
| 78 | BROKEN: unresolved name in main | error | 66 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 79 | fixed again by adding the missing function | full | 168 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 76 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 81 | fixed again: caller updated | full | 175 | same |
| 82 | BROKEN: file emptied | error | 91 | survived (error: `main` has errors, `cargo check` shows them: frontend) |
| 83 | file restored and three crates edited in one build | full | 210 | same |
| 84 | whole file rewritten in another order | patch | 43 | same |
| 85 | large generated function added | patch | 607 | same |
| 86 | everything back to the first source | full | 161 | same |
| 87 | and forward again to the last source | full | 703 | same |
