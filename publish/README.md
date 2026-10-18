# Publishing

`rustd-collate` is rust-analyzer's tree with rustd's changes made in place, which is the
layout the work was done in. The published `main` branch has a different layout: rustd's own
files, rust-analyzer as a submodule pinned to the commit rustd forked from, and a patch
series for the rust-analyzer files rustd changes.

`python3 publish/split.py SRC OUT` writes that layout into OUT from a checkout of this
branch. Files under `publish/overlay/` are copied over the result last: the README, the docs,
the workflows and `vendor/apply.py` of the published tree live there and are edited there.
