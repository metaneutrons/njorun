# Vendored build inputs

`i-slint-compiler/` is `i-slint-compiler 1.18.1` from crates.io with one change, taken from
[slint-ui/slint#13738](https://github.com/slint-ui/slint/pull/13738) (merged 2026-09-30, not yet released):
the characters and imported fonts that `EmbedForSoftwareRenderer` embeds are kept in a `BTreeSet` and a
`BTreeMap` instead of a `HashSet` and a `HashMap`. Without it, the glyph indices differ from one compilation
to the next and the P4 firmware is not reproducible once fonts are bundled.

Changed files: `passes.rs`, `passes/embed_glyphs.rs`. The crate's `tests/` and `benches/` were left out and
their manifest entries removed. Delete this directory and the `[patch.crates-io]` entry in `../Cargo.toml`
when Slint 1.19 is released and pinned.


`espidf.constraints.v6.1.txt` is the unchanged Espressif snapshot originally
served from <https://dl.espressif.com/dl/esp-idf/espidf.constraints.v6.1.txt>.
Its generator records `release/v6.1@baf4889fa2488afeff5210a53e16e7e3277b8b10`
and uv 0.13.7. Its SHA-256 matches the original qualification pin in
`../build.sh`. Keeping the snapshot here lets the build verify it before pip
reads it, independently of updates to Espressif's download URL.
