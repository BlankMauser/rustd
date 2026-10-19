# Stress edits

99 of 99 steps right, 0 restarts. Log: /tmp/rustd-stress-out-56ud412b/daemon.log

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | body: constant in a leaf function | patch | 8 | same |
| 1 | body: in main | patch | 25 | same |
| 2 | body: generic function used from another crate | patch | 18 | same |
| 3 | body: generic method of a generic struct | patch | 12 | same |
| 4 | body: trait default method | patch | 11 | same |
| 5 | body: closure inside a function | patch | 17 | same |
| 6 | body: async fn | patch | 11 | same |
| 7 | body: async fn gets another await | patch | 18 | same |
| 8 | body: Drop impl | patch | 13 | same |
| 9 | body: match arm in trait impl | patch | 10 | same |
| 10 | blank line at top of a file with line!() and track_caller | patch | 10 | same |
| 11 | blank line above a panic site | patch | 10 | same |
| 12 | comment appended | patch | 10 | same |
| 13 | new private function, unused | patch | 10 | same |
| 14 | new public function, called from main | patch | 71 | same |
| 15 | function renamed, callers updated | patch | 51 | same |
| 16 | function removed | patch | 36 | same |
| 17 | function moved to another file of the crate | patch | 54 | same |
| 18 | signature: parameter added, callers in another crate updated | patch | 61 | same |
| 19 | signature: return type changed | patch | 52 | same |
| 20 | signature: parameter type &str to String | patch | 51 | same |
| 21 | signature: function made generic | full | 107 | same |
| 22 | signature: impl Fn return becomes Box<dyn Fn> | patch | 33 | same |
| 23 | signature: const fn body and a const that uses it | patch | 22 | same |
| 24 | signature: fn becomes async, caller awaits through block_on | patch | 32 | same |
| 25 | struct: field added, constructor updated | patch | 35 | same |
| 26 | struct: fields reordered (derived PartialOrd changes) | patch | 38 | same |
| 27 | struct: field type widened across crates | patch | 33 | same |
| 28 | struct: private field added to a generic struct | patch | 34 | same |
| 29 | enum: variant added, matches updated | patch | 38 | same |
| 30 | enum: variants reordered (discriminants move) | patch | 37 | same |
| 31 | derive added | patch | 41 | same |
| 32 | derive removed, hand-written impl instead | full | 103 | same |
| 33 | repr and type alias | patch | 35 | same |
| 34 | trait: method added with a default (vtable grows) | patch | 33 | same |
| 35 | trait: default overridden in one impl | patch | 22 | same |
| 36 | trait: required method added, all impls updated | patch | 37 | same |
| 37 | trait: associated const changed | patch | 16 | same |
| 38 | trait: new impl for a foreign-crate type | patch | 47 | same |
| 39 | trait: impl removed | patch | 34 | same |
| 40 | trait: new trait with blanket impl | patch | 35 | same |
| 41 | trait: generic bound tightened | patch | 32 | same |
| 42 | trait: associated type changed on an Iterator impl | patch | 42 | same |
| 43 | trait: operator impl added | patch | 35 | same |
| 44 | generic: new instantiation of an existing generic | patch | 35 | same |
| 45 | generic: std collection never used before | patch | 225 | same |
| 46 | generic: type parameter added to a function | patch | 73 | same |
| 47 | generic: const generic introduced | patch | 68 | same |
| 48 | macro: body of macro_rules used in another crate | patch | 52 | same |
| 49 | macro: function-making macro changed | patch | 52 | same |
| 50 | macro: new invocation makes a new function | patch | 71 | same |
| 51 | macro: new macro defined and used in the same edit | patch | 77 | same |
| 52 | macro: macro that declares a struct and impl | patch | 75 | same |
| 53 | macro: format string changed | patch | 56 | same |
| 54 | macro: matches!, vec!, assert!, write! in a new function | patch | 81 | same |
| 55 | const value changed (used in a const fn and an array length) | patch | 72 | same |
| 56 | static value changed | patch | 57 | same |
| 57 | static added and used | patch | 74 | same |
| 58 | static initial value of an atomic changed | patch | 54 | same |
| 59 | thread_local added | patch | 105 | same |
| 60 | static with a lazily made value | patch | 153 | same |
| 61 | new file and module | patch | 105 | same |
| 62 | edit to the file just added | patch | 72 | same |
| 63 | module renamed (file moved) | patch | 102 | same |
| 64 | module becomes a directory with a submodule | patch | 98 | same |
| 65 | re-export added and used | patch | 101 | same |
| 66 | inline module with nested function and impl | patch | 97 | same |
| 67 | include_str! of a new file | patch | 116 | same |
| 68 | the included file changes | patch | 83 | same |
| 69 | module removed | patch | 101 | same |
| 70 | #[inline(never)] and #[cold] added | patch | 78 | same |
| 71 | cfg(debug_assertions) branch | patch | 117 | same |
| 72 | visibility narrowed and widened | patch | 92 | same |
| 73 | BROKEN: syntax error | error | 300 | said so (error: /tmp/rustd-stress-g8gdxnvg/mid/src/text.rs:17: expect) |
| 74 | fixed again, with a body change | full | 95 | same |
| 75 | BROKEN: type error in a body | error | 89 | said so (error: `main` is not compiled by rustd: unsupported: callee ) |
| 76 | BROKEN: still broken, another file edited too | error | 10 | said so (error: `main` is not compiled by rustd: unsupported: callee ) |
| 77 | fixed again | full | 75 | same |
| 78 | BROKEN: unresolved name in main | error | 49 | said so (error: `main` has an error rustd cannot say more of: fronten) |
| 79 | fixed again by adding the missing function | full | 95 | same |
| 80 | BROKEN: caller not updated after a signature change | error | 48 | said so (error: `main` has an error rustd cannot say more of: fronten) |
| 81 | fixed again: caller updated | full | 99 | same |
| 82 | BROKEN: file emptied | error | 49 | said so (error: `main` has an error rustd cannot say more of: fronten) |
| 83 | file restored and three crates edited in one build | full | 103 | same |
| 84 | thread_local of the first source given another value | full | 81 | same |
| 85 | BROKEN: struct literal without one of its fields | error | 10 | said so (error: /tmp/rustd-stress-g8gdxnvg/base/src/lib.rs:109:5: mis) |
| 86 | mended: the field given | patch | 8 | same |
| 87 | BROKEN: match without one of its arms | error | 24 | said so (error: /tmp/rustd-stress-g8gdxnvg/mid/src/lib.rs:199:11: mis) |
| 88 | mended: the arm given | patch | 10 | same |
| 89 | BROKEN: impl without one of its trait's methods | error | 22 | said so (error: /tmp/rustd-stress-g8gdxnvg/app/src/main.rs:106:6: not) |
| 90 | mended: the method given | patch | 12 | same |
| 91 | BROKEN: method that no type has | error | 10 | said so (error: /tmp/rustd-stress-g8gdxnvg/mid/src/text.rs:22:7: no m) |
| 92 | mended: the method named right | patch | 7 | same |
| 93 | BROKEN: import of a name that is not there | error | 36 | said so (error: /tmp/rustd-stress-g8gdxnvg/app/src/main.rs:102:5: unr) |
| 94 | mended: the import removed | patch | 35 | same |
| 95 | whole file rewritten in another order | patch | 10 | same |
| 96 | large generated function added | patch | 150 | same |
| 97 | everything back to the first source | full | 135 | same |
| 98 | and forward again to the last source | full | 176 | same |
