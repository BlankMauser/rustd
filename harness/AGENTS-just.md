# Agent-style edits: just, seed 1

80 of 80 steps right, 0 restarts, 77 patches (median 575 ms with the link), 0 full objects, rss 1953 MB at the end. Warmup 24.7 s.

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | unit 0 added (enum) | patch | 661 | same |
| 1 | unit 1 added (macro) | patch | 642 | same |
| 2 | unit 2 added (async) | patch | 555 | same |
| 3 | unit 1 (macro) to version 1 | patch | 524 | same |
| 4 | unit 0 (enum) to version 1 | patch | 557 | same |
| 5 | unit 3 added (async) | patch | 636 | same |
| 6 | unit 4 added (closure) | patch | 648 | same |
| 7 | unit 4 (closure) to version 1 | patch | 526 | same |
| 8 | unit 5 added (async) | patch | 538 | same |
| 9 | BROKEN: unresolved name in src/compiler.rs | error | 265 | said so |
| 10 | mended | patch | 574 | same |
| 11 | unit 6 added (static) | patch | 1146 | same |
| 12 | unit 7 added (plain) | patch | 632 | same |
| 13 | unit 8 added (collections) | patch | 613 | same |
| 14 | unit 9 added (closure) | patch | 606 | same |
| 15 | blank in src/compiler.rs | patch | 506 | same |
| 16 | burst: unit 8 (collections) to version 1; unit 11 added (static); unit 12 added (generic); unit 7 (plain) to version 1; fn in src/main.rs | patch | 561 | same |
| 17 | unit 14 added (trait) | patch | 545 | same |
| 18 | unit 1 (macro) to version 2 | patch | 485 | same |
| 19 | unit 12 (generic) to version 1 | patch | 442 | same |
| 20 | unit 8 (collections) to version 2 | patch | 391 | same |
| 21 | let in src/lib.rs | patch | 419 | same |
| 22 | unit 9 (closure) to version 1 | patch | 626 | same |
| 23 | unit 16 added (async) | patch | 539 | same |
| 24 | burst: let in src/run.rs; unit 18 added (async); unit 8 (collections) to version 3; unit 19 added (trait); unit 14 (trait) to version 1 | patch | 575 | same |
| 25 | back to the first source | patch | 553 | same |
| 26 | forward again | patch | 588 | same |
| 27 | unit 19 (trait) to version 1 | patch | 446 | same |
| 28 | unit 20 added (async) | patch | 570 | same |
| 29 | unit 21 added (macro) | patch | 575 | same |
| 30 | unit 5 (async) to version 1 | patch | 512 | same |
| 31 | comment in src/config.rs | patch | 419 | same |
| 32 | unit 23 added (generic) | patch | 604 | same |
| 33 | burst: unit 12 (generic) to version 2; unit 23 (generic) removed; let in src/compiler.rs; unit 0 (enum) to version 2; comment in src/config.rs | patch | 631 | same |
| 34 | blank in src/compiler.rs | patch | 510 | same |
| 35 | unit 12 (generic) removed | patch | 571 | same |
| 36 | fn in src/config.rs | patch | 538 | same |
| 37 | unit 14 (trait) removed | patch | 669 | same |
| 38 | unit 16 (async) to version 1 | patch | 503 | same |
| 39 | unit 28 added (collections) | patch | 584 | same |
| 40 | unit 20 (async) to version 1 | patch | 454 | same |
| 41 | unit 29 added (generic) | patch | 555 | same |
| 42 | comment in src/config.rs | patch | 564 | same |
| 43 | unit 31 added (collections) | patch | 580 | same |
| 44 | BROKEN: unresolved name in src/run.rs | error | 138 | said so |
| 45 | mended | patch | 723 | same |
| 46 | unit 32 added (struct) | patch | 1101 | same |
| 47 | unit 33 added (async) | patch | 877 | same |
| 48 | unit 34 added (macro) | patch | 760 | same |
| 49 | burst: unit 35 added (enum); unit 36 added (static); comment in src/main.rs; unit 38 added (macro) | patch | 981 | same |
| 50 | back to the first source | patch | 757 | same |
| 51 | forward again | patch | 753 | same |
| 52 | let in src/lib.rs | patch | 652 | same |
| 53 | unit 21 (macro) to version 1 | patch | 545 | same |
| 54 | unit 3 (async) removed | patch | 719 | same |
| 55 | unit 40 added (macro) | patch | 796 | same |
| 56 | unit 16 (async) to version 2 | patch | 677 | same |
| 57 | unit 7 (plain) to version 2 | patch | 574 | same |
| 58 | BROKEN: type error in src/compiler.rs | error | 170 | said so |
| 59 | mended | patch | 662 | same |
| 60 | fn in src/config.rs | patch | 515 | same |
| 61 | unit 42 added (generic) | patch | 652 | same |
| 62 | unit 43 added (closure) | patch | 653 | same |
| 63 | comment in src/main.rs | patch | 463 | same |
| 64 | unit 36 (static) to version 1 | patch | 662 | same |
| 65 | unit 31 (collections) to version 1 | patch | 510 | same |
| 66 | burst: unit 45 added (collections); unit 0 (enum) to version 3; let in src/config.rs; unit 20 (async) to version 2; unit 47 added (macro) | patch | 629 | same |
| 67 | blank in src/run.rs | patch | 620 | same |
| 68 | unit 2 (async) to version 1 | patch | 510 | same |
| 69 | unit 49 added (macro) | patch | 746 | same |
| 70 | unit 38 (macro) to version 1 | patch | 670 | same |
| 71 | let in src/compiler.rs | patch | 634 | same |
| 72 | unit 51 added (collections) | patch | 904 | same |
| 73 | unit 52 added (enum) | patch | 929 | same |
| 74 | fn in src/config.rs | patch | 514 | same |
| 75 | back to the first source | patch | 463 | same |
| 76 | forward again | patch | 1147 | same |
| 77 | blank in src/config.rs | patch | 448 | same |
| 78 | unit 19 (trait) to version 2 | patch | 453 | same |
| 79 | unit 7 (plain) to version 3 | patch | 542 | same |
