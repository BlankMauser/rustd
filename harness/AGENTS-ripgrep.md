# Agent-style edits: ripgrep, seed 1

80 of 80 steps right, 0 restarts, 76 patches (median 718 ms with the link), 0 full objects, rss 1281 MB at the end. Warmup 12.7 s.

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | unit 0 added (enum) | patch | 1112 | same |
| 1 | unit 1 added (macro) | patch | 721 | same |
| 2 | unit 2 added (async) | patch | 793 | same |
| 3 | unit 1 (macro) to version 1 | patch | 330 | same |
| 4 | unit 0 (enum) to version 1 | patch | 287 | same |
| 5 | unit 3 added (async) | patch | 777 | same |
| 6 | unit 4 added (closure) | patch | 718 | same |
| 7 | unit 4 (closure) to version 1 | patch | 327 | same |
| 8 | unit 5 added (async) | patch | 1361 | same |
| 9 | BROKEN: syntax error in crates/core/rustd_u3.rs | error | 115 | said so |
| 10 | mended | patch | 292 | same |
| 11 | unit 6 added (static) | patch | 763 | same |
| 12 | unit 7 added (plain) | patch | 805 | same |
| 13 | unit 8 added (collections) | patch | 1318 | same |
| 14 | unit 9 added (closure) | patch | 1780 | same |
| 15 | blank in crates/printer/src/standard.rs | patch | 397 | same |
| 16 | library function 11 added | patch | 346 | same |
| 17 | burst: library function 11 to version 1; unit 3 (async) to version 1; library function 12 added; unit 13 added (closure); unit 14 added (trait) | patch | 808 | same |
| 18 | unit 1 (macro) to version 2 | patch | 627 | same |
| 19 | unit 14 (trait) to version 1 | patch | 387 | same |
| 20 | library function 15 added | patch | 343 | same |
| 21 | unit 8 (collections) to version 1 | patch | 311 | same |
| 22 | let in crates/searcher/src/searcher/core.rs | patch | 454 | same |
| 23 | unit 8 (collections) removed | patch | 1198 | same |
| 24 | unit 13 (closure) to version 1 | patch | 496 | same |
| 25 | back to the first source | patch | 1095 | same |
| 26 | forward again | patch | 1182 | same |
| 27 | unit 17 added (async) | patch | 881 | same |
| 28 | burst: unit 14 (trait) removed; let in crates/searcher/src/searcher/core.rs; library function 19 added; unit 20 added (trait); unit 5 (async) to version 1 | patch | 984 | same |
| 29 | unit 20 (trait) removed | patch | 958 | same |
| 30 | unit 21 added (async) | patch | 847 | same |
| 31 | unit 22 added (macro) | patch | 929 | same |
| 32 | unit 17 (async) to version 1 | patch | 338 | same |
| 33 | comment in crates/regex/src/literal.rs | patch | 325 | same |
| 34 | unit 24 added (generic) | patch | 859 | same |
| 35 | library function 25 added | patch | 357 | same |
| 36 | library function 25 to version 1 | patch | 436 | same |
| 37 | unit 24 (generic) to version 1 | patch | 415 | same |
| 38 | unit 7 (plain) to version 1 | patch | 378 | same |
| 39 | BROKEN: unresolved name in crates/core/rustd_u24.rs | error | 137 | said so |
| 40 | mended | patch | 465 | same |
| 41 | unit 26 added (closure) | patch | 806 | same |
| 42 | library function 27 added | patch | 455 | same |
| 43 | library function 28 added | patch | 381 | same |
| 44 | blank in crates/printer/src/standard.rs | patch | 610 | same |
| 45 | unit 24 (generic) removed | patch | 1273 | same |
| 46 | unit 13 (closure) to version 2 | patch | 587 | same |
| 47 | library function 25 to version 2 | patch | 472 | same |
| 48 | unit 7 (plain) to version 2 | patch | 536 | same |
| 49 | unit 6 (static) to version 1 | patch | 479 | same |
| 50 | back to the first source | patch | 1247 | same |
| 51 | forward again | patch | 1474 | same |
| 52 | unit 0 (enum) to version 2 | patch | 428 | same |
| 53 | library function 28 to version 1 | patch | 607 | same |
| 54 | unit 7 (plain) to version 3 | patch | 550 | same |
| 55 | unit 30 added (generic) | patch | 898 | same |
| 56 | comment in crates/ignore/src/walk.rs | patch | 592 | same |
| 57 | unit 32 added (collections) | patch | 1080 | same |
| 58 | BROKEN: unresolved name in crates/regex/src/literal.rs | error | 177 | said so |
| 59 | mended | patch | 371 | same |
| 60 | unit 33 added (struct) | patch | 1163 | same |
| 61 | unit 34 added (async) | patch | 979 | same |
| 62 | unit 35 added (macro) | patch | 1116 | same |
| 63 | burst: unit 36 added (enum); let in crates/searcher/src/searcher/core.rs; let in crates/regex/src/literal.rs; let in crates/globset/src/glob.rs | patch | 1695 | same |
| 64 | unit 30 (generic) to version 1 | patch | 593 | same |
| 65 | unit 32 (collections) to version 1 | patch | 524 | same |
| 66 | unit 3 (async) removed | patch | 1269 | same |
| 67 | unit 40 added (macro) | patch | 1327 | same |
| 68 | unit 21 (async) to version 1 | patch | 762 | same |
| 69 | unit 7 (plain) to version 4 | patch | 535 | same |
| 70 | BROKEN: type error in crates/printer/src/standard.rs | error | 270 | said so |
| 71 | mended | patch | 641 | same |
| 72 | library function 41 added | patch | 729 | same |
| 73 | unit 0 (enum) to version 3 | patch | 582 | same |
| 74 | burst: unit 5 (async) to version 2; unit 42 added (trait) | patch | 1767 | same |
| 75 | back to the first source | patch | 1267 | same |
| 76 | forward again | patch | 1617 | same |
| 77 | unit 34 (async) to version 1 | patch | 672 | same |
| 78 | unit 35 (macro) to version 1 | patch | 677 | same |
| 79 | burst: unit 34 (async) to version 2; unit 43 added (closure); unit 43 (closure) to version 1; unit 1 (macro) removed; unit 6 (static) to version 2 | patch | 1190 | same |
