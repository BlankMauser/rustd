# Agent-style edits: ripgrep, seed 1

80 of 80 steps right, 0 restarts, 76 patches (median 303 ms with the link), 0 full objects, rss 1323 MB at the end. Warmup 10.5 s.

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | unit 0 added (enum) | patch | 295 | same |
| 1 | unit 1 added (macro) | patch | 262 | same |
| 2 | unit 2 added (async) | patch | 286 | same |
| 3 | unit 1 (macro) to version 1 | patch | 268 | same |
| 4 | unit 0 (enum) to version 1 | patch | 279 | same |
| 5 | unit 3 added (async) | patch | 268 | same |
| 6 | unit 4 added (closure) | patch | 270 | same |
| 7 | unit 4 (closure) to version 1 | patch | 276 | same |
| 8 | unit 5 added (async) | patch | 282 | same |
| 9 | BROKEN: syntax error in crates/core/rustd_u3.rs | error | 60 | said so |
| 10 | mended | patch | 283 | same |
| 11 | unit 6 added (static) | patch | 801 | same |
| 12 | unit 7 added (plain) | patch | 304 | same |
| 13 | unit 8 added (collections) | patch | 376 | same |
| 14 | unit 9 added (closure) | patch | 390 | same |
| 15 | blank in crates/printer/src/standard.rs | patch | 346 | same |
| 16 | library function 11 added | patch | 323 | same |
| 17 | burst: library function 11 to version 1; unit 3 (async) to version 1; library function 12 added; unit 13 added (closure); unit 14 added (trait) | patch | 298 | same |
| 18 | unit 1 (macro) to version 2 | patch | 268 | same |
| 19 | unit 14 (trait) to version 1 | patch | 264 | same |
| 20 | library function 15 added | patch | 282 | same |
| 21 | unit 8 (collections) to version 1 | patch | 307 | same |
| 22 | let in crates/searcher/src/searcher/core.rs | patch | 276 | same |
| 23 | unit 8 (collections) removed | patch | 350 | same |
| 24 | unit 13 (closure) to version 1 | patch | 542 | same |
| 25 | back to the first source | patch | 358 | same |
| 26 | forward again | patch | 459 | same |
| 27 | unit 17 added (async) | patch | 285 | same |
| 28 | burst: unit 14 (trait) removed; let in crates/searcher/src/searcher/core.rs; library function 19 added; unit 20 added (trait); unit 5 (async) to version 1 | patch | 295 | same |
| 29 | unit 20 (trait) removed | patch | 342 | same |
| 30 | unit 21 added (async) | patch | 292 | same |
| 31 | unit 22 added (macro) | patch | 355 | same |
| 32 | unit 17 (async) to version 1 | patch | 356 | same |
| 33 | comment in crates/regex/src/literal.rs | patch | 228 | same |
| 34 | unit 24 added (generic) | patch | 320 | same |
| 35 | library function 25 added | patch | 353 | same |
| 36 | library function 25 to version 1 | patch | 380 | same |
| 37 | unit 24 (generic) to version 1 | patch | 234 | same |
| 38 | unit 7 (plain) to version 1 | patch | 317 | same |
| 39 | BROKEN: unresolved name in crates/core/rustd_u24.rs | error | 59 | said so |
| 40 | mended | patch | 299 | same |
| 41 | unit 26 added (closure) | patch | 289 | same |
| 42 | library function 27 added | patch | 282 | same |
| 43 | library function 28 added | patch | 302 | same |
| 44 | blank in crates/printer/src/standard.rs | patch | 256 | same |
| 45 | unit 24 (generic) removed | patch | 290 | same |
| 46 | unit 13 (closure) to version 2 | patch | 309 | same |
| 47 | library function 25 to version 2 | patch | 332 | same |
| 48 | unit 7 (plain) to version 2 | patch | 307 | same |
| 49 | unit 6 (static) to version 1 | patch | 303 | same |
| 50 | back to the first source | patch | 340 | same |
| 51 | forward again | patch | 411 | same |
| 52 | unit 0 (enum) to version 2 | patch | 354 | same |
| 53 | library function 28 to version 1 | patch | 379 | same |
| 54 | unit 7 (plain) to version 3 | patch | 301 | same |
| 55 | unit 30 added (generic) | patch | 351 | same |
| 56 | comment in crates/ignore/src/walk.rs | patch | 298 | same |
| 57 | unit 32 added (collections) | patch | 402 | same |
| 58 | BROKEN: unresolved name in crates/regex/src/literal.rs | error | 99 | said so |
| 59 | mended | patch | 332 | same |
| 60 | unit 33 added (struct) | patch | 313 | same |
| 61 | unit 34 added (async) | patch | 345 | same |
| 62 | unit 35 added (macro) | patch | 317 | same |
| 63 | burst: unit 36 added (enum); let in crates/searcher/src/searcher/core.rs; let in crates/regex/src/literal.rs; let in crates/globset/src/glob.rs | patch | 363 | same |
| 64 | unit 30 (generic) to version 1 | patch | 283 | same |
| 65 | unit 32 (collections) to version 1 | patch | 262 | same |
| 66 | unit 3 (async) removed | patch | 297 | same |
| 67 | unit 40 added (macro) | patch | 362 | same |
| 68 | unit 21 (async) to version 1 | patch | 266 | same |
| 69 | unit 7 (plain) to version 4 | patch | 348 | same |
| 70 | BROKEN: type error in crates/printer/src/standard.rs | error | 122 | said so |
| 71 | mended | patch | 306 | same |
| 72 | library function 41 added | patch | 315 | same |
| 73 | unit 0 (enum) to version 3 | patch | 276 | same |
| 74 | burst: unit 5 (async) to version 2; unit 42 added (trait) | patch | 261 | same |
| 75 | back to the first source | patch | 285 | same |
| 76 | forward again | patch | 422 | same |
| 77 | unit 34 (async) to version 1 | patch | 259 | same |
| 78 | unit 35 (macro) to version 1 | patch | 236 | same |
| 79 | burst: unit 34 (async) to version 2; unit 43 added (closure); unit 43 (closure) to version 1; unit 1 (macro) removed; unit 6 (static) to version 2 | patch | 288 | same |
