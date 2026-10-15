# Agent-style edits: sapp, seed 2

60 of 60 steps right, 0 restarts, 59 patches (median 88 ms with the link), 0 full objects, rss 664 MB at the end. Warmup 2.4 s.

| # | Edit | Reply | ms | Result |
|---|---|---|---:|---|
| 0 | unit 0 added (struct) | patch | 82 | same |
| 1 | unit 1 added (static) | patch | 88 | same |
| 2 | let in mid/src/text.rs | patch | 37 | same |
| 3 | blank in mid/src/text.rs | patch | 42 | same |
| 4 | fn in base/src/lib.rs | patch | 37 | same |
| 5 | library function 5 added | patch | 99 | same |
| 6 | unit 1 (static) to version 1 | patch | 100 | same |
| 7 | library function 5 to version 1 | patch | 87 | same |
| 8 | unit 6 added (static) | patch | 104 | same |
| 9 | unit 1 (static) to version 2 | patch | 46 | same |
| 10 | unit 1 (static) to version 3 | patch | 45 | same |
| 11 | library function 5 to version 2 | patch | 86 | same |
| 12 | burst: unit 7 added (trait); unit 1 (static) to version 4; let in mid/src/text.rs | patch | 97 | same |
| 13 | library function 5 to version 3 | patch | 87 | same |
| 14 | unit 6 (static) to version 1 | patch | 50 | same |
| 15 | unit 6 (static) to version 2 | patch | 40 | same |
| 16 | unit 1 (static) to version 5 | patch | 50 | same |
| 17 | unit 7 (trait) to version 1 | patch | 50 | same |
| 18 | library function 5 to version 4 | patch | 102 | same |
| 19 | unit 6 (static) removed | patch | 97 | same |
| 20 | unit 1 (static) to version 6 | patch | 47 | same |
| 21 | unit 7 (trait) to version 2 | patch | 44 | same |
| 22 | library function 9 added | patch | 100 | same |
| 23 | unit 7 (trait) removed | patch | 91 | same |
| 24 | burst: let in mid/src/text.rs; unit 1 (static) to version 7; unit 1 (static) removed; let in base/src/lib.rs | patch | 85 | same |
| 25 | back to the first source | patch | 80 | same |
| 26 | forward again | patch | 168 | same |
| 27 | unit 12 added (static) | patch | 120 | same |
| 28 | unit 13 added (generic) | patch | 122 | same |
| 29 | unit 14 added (plain) | patch | 133 | same |
| 30 | unit 15 added (macro) | patch | 116 | same |
| 31 | burst: comment in base/src/lib.rs; fn in app/src/main.rs | patch | 52 | same |
| 32 | unit 18 added (plain) | patch | 96 | same |
| 33 | unit 13 (generic) to version 1 | patch | 51 | same |
| 34 | blank in app/src/main.rs | patch | 38 | same |
| 35 | unit 20 added (struct) | patch | 100 | same |
| 36 | unit 21 added (plain) | patch | 97 | same |
| 37 | unit 22 added (plain) | patch | 103 | same |
| 38 | unit 15 (macro) to version 1 | patch | 44 | same |
| 39 | comment in app/src/main.rs | patch | 40 | same |
| 40 | library function 24 added | patch | 101 | same |
| 41 | unit 0 (struct) to version 1 | patch | 102 | same |
| 42 | burst: unit 25 added (plain); unit 25 (plain) to version 1; unit 26 added (macro) | patch | 103 | same |
| 43 | unit 21 (plain) to version 1 | patch | 94 | same |
| 44 | unit 27 added (macro) | patch | 105 | same |
| 45 | unit 22 (plain) to version 1 | patch | 96 | same |
| 46 | unit 28 added (macro) | patch | 106 | same |
| 47 | unit 25 (plain) to version 2 | patch | 99 | same |
| 48 | comment in base/src/lib.rs | patch | 41 | same |
| 49 | unit 30 added (static) | patch | 111 | same |
| 50 | back to the first source | patch | 79 | same |
| 51 | forward again | patch | 124 | same |
| 52 | unit 31 added (plain) | patch | 114 | same |
| 53 | unit 28 (macro) to version 1 | patch | 42 | same |
| 54 | fn in mid/src/text.rs | patch | 48 | same |
| 55 | unit 22 (plain) removed | patch | 114 | same |
| 56 | unit 13 (generic) to version 2 | patch | 55 | same |
| 57 | unit 15 (macro) to version 2 | patch | 49 | same |
| 58 | BROKEN: unresolved name in app/src/rustd_u26.rs | error | 17 | said so |
| 59 | mended | patch | 49 | same |
