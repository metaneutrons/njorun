# Vendored crates

`i-slint-compiler/` is `i-slint-compiler 1.18.1` from crates.io with one change, taken from
[slint-ui/slint#13738](https://github.com/slint-ui/slint/pull/13738) (merged 2026-09-30, not yet released):
the characters and imported fonts that `EmbedForSoftwareRenderer` embeds are kept in a `BTreeSet` and a
`BTreeMap` instead of a `HashSet` and a `HashMap`. Without it, the glyph indices differ from one compilation
to the next and the P4 firmware is not reproducible once fonts are bundled.

Changed files: `passes.rs`, `passes/embed_glyphs.rs`. The crate's `tests/` and `benches/` were left out and
their manifest entries removed. Delete this directory and the `[patch.crates-io]` entry in `../Cargo.toml`
when Slint 1.19 is released and pinned.
