# Agent-style edits: sapp, seed 1

60 of 60 steps right, 0 restarts, 57 patches (median 181 ms with the link), 1 full objects, rss 691 MB at the end. Warmup 2.7 s.

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | unit 0 added (enum) | patch | 286 | same |
| 1 | unit 1 added (macro) | patch | 231 | same |
| 2 | unit 2 added (async) | patch | 174 | same |
| 3 | unit 1 (macro) to version 1 | patch | 86 | same |
| 4 | unit 0 (enum) to version 1 | patch | 94 | same |
| 5 | unit 3 added (async) | patch | 254 | same |
| 6 | unit 4 added (closure) | patch | 282 | same |
| 7 | unit 4 (closure) to version 1 | patch | 139 | same |
| 8 | unit 5 added (async) | patch | 232 | same |
| 9 | BROKEN: syntax error in app/src/rustd_u4.rs | error | 324 | said so |
| 10 | mended | full | 102 | same |
| 11 | unit 6 added (static) | patch | 159 | same |
| 12 | unit 7 added (plain) | patch | 142 | same |
| 13 | unit 8 added (collections) | patch | 443 | same |
| 14 | unit 9 added (closure) | patch | 334 | same |
| 15 | blank in app/src/main.rs | patch | 82 | same |
| 16 | burst: unit 8 (collections) to version 1; unit 11 added (static); unit 12 added (generic); unit 7 (plain) to version 1; fn in app/src/main.rs | patch | 286 | same |
| 17 | unit 14 added (trait) | patch | 257 | same |
| 18 | unit 1 (macro) to version 2 | patch | 115 | same |
| 19 | unit 12 (generic) to version 1 | patch | 115 | same |
| 20 | unit 8 (collections) to version 2 | patch | 121 | same |
| 21 | let in app/src/main.rs | patch | 105 | same |
| 22 | unit 9 (closure) to version 1 | patch | 109 | same |
| 23 | unit 16 added (async) | patch | 329 | same |
| 24 | burst: let in app/src/main.rs; unit 18 added (async); unit 8 (collections) to version 3; unit 19 added (trait); unit 14 (trait) to version 1 | patch | 265 | same |
| 25 | back to the first source | patch | 138 | same |
| 26 | forward again | patch | 272 | same |
| 27 | unit 19 (trait) to version 1 | patch | 159 | same |
| 28 | unit 20 added (async) | patch | 458 | same |
| 29 | unit 21 added (macro) | patch | 331 | same |
| 30 | unit 5 (async) to version 1 | patch | 148 | same |
| 31 | blank in app/src/main.rs | patch | 126 | same |
| 32 | fn in app/src/main.rs | patch | 149 | same |
| 33 | unit 12 (generic) to version 2 | patch | 150 | same |
| 34 | unit 8 (collections) to version 4 | patch | 119 | same |
| 35 | unit 24 added (closure) | patch | 302 | same |
| 36 | fn in app/src/main.rs | patch | 139 | same |
| 37 | unit 26 added (async) | patch | 347 | same |
| 38 | unit 24 (closure) to version 1 | patch | 141 | same |
| 39 | fn in app/src/main.rs | patch | 160 | same |
| 40 | unit 16 (async) to version 1 | patch | 181 | same |
| 41 | unit 0 (enum) to version 2 | patch | 158 | same |
| 42 | unit 18 (async) to version 1 | patch | 140 | same |
| 43 | unit 28 added (generic) | patch | 317 | same |
| 44 | blank in app/src/main.rs | patch | 110 | same |
| 45 | BROKEN: unresolved name in app/src/rustd_u0.rs | error | 91 | said so |
| 46 | mended | patch | 124 | same |
| 47 | unit 30 added (struct) | patch | 481 | same |
| 48 | unit 31 added (async) | patch | 522 | same |
| 49 | unit 32 added (macro) | patch | 507 | same |
| 50 | back to the first source | patch | 215 | same |
| 51 | forward again | patch | 458 | same |
| 52 | burst: unit 33 added (enum); unit 34 added (static); comment in app/src/main.rs; unit 36 added (macro) | patch | 531 | same |
| 53 | let in app/src/main.rs | patch | 194 | same |
| 54 | unit 19 (trait) to version 2 | patch | 262 | same |
| 55 | unit 3 (async) removed | patch | 547 | same |
| 56 | unit 38 added (macro) | patch | 487 | same |
| 57 | unit 12 (generic) to version 3 | patch | 157 | same |
| 58 | unit 36 (macro) to version 1 | patch | 193 | same |
| 59 | blank in app/src/main.rs | patch | 141 | same |
