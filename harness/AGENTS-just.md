# Agent-style edits: just, seed 1

80 of 80 steps right, 0 restarts, 76 patches (median 448 ms with the link), 1 full objects, rss 2059 MB at the end. Warmup 16.6 s.

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | unit 0 added (enum) | patch | 537 | same |
| 1 | unit 1 added (macro) | patch | 460 | same |
| 2 | unit 2 added (async) | patch | 458 | same |
| 3 | unit 1 (macro) to version 1 | patch | 381 | same |
| 4 | unit 0 (enum) to version 1 | patch | 387 | same |
| 5 | unit 3 added (async) | patch | 525 | same |
| 6 | unit 4 added (closure) | patch | 404 | same |
| 7 | unit 4 (closure) to version 1 | patch | 383 | same |
| 8 | unit 5 added (async) | patch | 410 | same |
| 9 | BROKEN: syntax error in src/rustd_u4.rs | error | 1947 | said so |
| 10 | mended | full | 973 | same |
| 11 | unit 6 added (static) | patch | 411 | same |
| 12 | unit 7 added (plain) | patch | 433 | same |
| 13 | unit 8 added (collections) | patch | 522 | same |
| 14 | unit 9 added (closure) | patch | 446 | same |
| 15 | blank in src/main.rs | patch | 335 | same |
| 16 | burst: unit 8 (collections) to version 1; unit 11 added (static); unit 12 added (generic); unit 7 (plain) to version 1; fn in src/main.rs | patch | 443 | same |
| 17 | unit 14 added (trait) | patch | 474 | same |
| 18 | unit 1 (macro) to version 2 | patch | 366 | same |
| 19 | unit 12 (generic) to version 1 | patch | 393 | same |
| 20 | unit 8 (collections) to version 2 | patch | 384 | same |
| 21 | let in src/main.rs | patch | 358 | same |
| 22 | unit 9 (closure) to version 1 | patch | 419 | same |
| 23 | unit 16 added (async) | patch | 665 | same |
| 24 | burst: let in src/main.rs; unit 18 added (async); unit 8 (collections) to version 3; unit 19 added (trait); unit 14 (trait) to version 1 | patch | 579 | same |
| 25 | back to the first source | patch | 536 | same |
| 26 | forward again | patch | 543 | same |
| 27 | unit 19 (trait) to version 1 | patch | 416 | same |
| 28 | unit 20 added (async) | patch | 607 | same |
| 29 | unit 21 added (macro) | patch | 455 | same |
| 30 | unit 5 (async) to version 1 | patch | 421 | same |
| 31 | blank in src/main.rs | patch | 356 | same |
| 32 | fn in src/main.rs | patch | 417 | same |
| 33 | unit 12 (generic) to version 2 | patch | 448 | same |
| 34 | unit 8 (collections) to version 4 | patch | 571 | same |
| 35 | unit 24 added (closure) | patch | 574 | same |
| 36 | fn in src/main.rs | patch | 530 | same |
| 37 | unit 26 added (async) | patch | 596 | same |
| 38 | unit 24 (closure) to version 1 | patch | 464 | same |
| 39 | fn in src/main.rs | patch | 406 | same |
| 40 | unit 16 (async) to version 1 | patch | 462 | same |
| 41 | unit 0 (enum) to version 2 | patch | 398 | same |
| 42 | unit 18 (async) to version 1 | patch | 387 | same |
| 43 | unit 28 added (generic) | patch | 487 | same |
| 44 | blank in src/main.rs | patch | 374 | same |
| 45 | BROKEN: unresolved name in src/rustd_u0.rs | error | 73 | said so |
| 46 | mended | patch | 401 | same |
| 47 | unit 30 added (struct) | patch | 517 | same |
| 48 | unit 31 added (async) | patch | 491 | same |
| 49 | unit 32 added (macro) | patch | 646 | same |
| 50 | back to the first source | patch | 491 | same |
| 51 | forward again | patch | 528 | same |
| 52 | burst: unit 33 added (enum); unit 34 added (static); comment in src/main.rs; unit 36 added (macro) | patch | 533 | same |
| 53 | let in src/main.rs | patch | 361 | same |
| 54 | unit 19 (trait) to version 2 | patch | 388 | same |
| 55 | unit 3 (async) removed | patch | 547 | same |
| 56 | unit 38 added (macro) | patch | 526 | same |
| 57 | unit 12 (generic) to version 3 | patch | 473 | same |
| 58 | unit 36 (macro) to version 1 | patch | 427 | same |
| 59 | blank in src/main.rs | patch | 381 | same |
| 60 | BROKEN: unresolved name in src/rustd_u32.rs | error | 66 | said so |
| 61 | mended | patch | 395 | same |
| 62 | blank in src/main.rs | patch | 345 | same |
| 63 | burst: blank in src/main.rs; unit 6 (static) to version 1 | patch | 419 | same |
| 64 | unit 32 (macro) to version 1 | patch | 411 | same |
| 65 | unit 24 (closure) to version 2 | patch | 436 | same |
| 66 | burst: unit 42 added (collections); unit 0 (enum) to version 3; fn in src/main.rs; unit 44 added (macro); unit 45 added (generic) | patch | 579 | same |
| 67 | unit 46 added (macro) | patch | 540 | same |
| 68 | unit 47 added (struct) | patch | 758 | same |
| 69 | unit 28 (generic) to version 1 | patch | 431 | same |
| 70 | let in src/main.rs | patch | 769 | same |
| 71 | unit 49 added (collections) | patch | 774 | same |
| 72 | unit 50 added (enum) | patch | 573 | same |
| 73 | comment in src/main.rs | patch | 402 | same |
| 74 | unit 52 added (closure) | patch | 751 | same |
| 75 | back to the first source | patch | 410 | same |
| 76 | forward again | patch | 604 | same |
| 77 | blank in src/main.rs | patch | 379 | same |
| 78 | comment in src/main.rs | patch | 400 | same |
| 79 | unit 7 (plain) removed | patch | 589 | same |
