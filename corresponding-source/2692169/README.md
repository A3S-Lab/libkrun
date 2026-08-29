# Corresponding wrapper source for the current A3S prebuilt

This directory preserves the exact minimal Rust build source used for the A3S
`libkrunfw.dll` whose SHA-256 is:

`44f25540f58155c01258fe123617636fdc6cff27873e38e71dbc75f139602077`

The files are copied byte-for-byte from libkrun commit
`2692169b7567363244fdd21cb83de3220ebf3021`. They are retained as corresponding
source and are deliberately outside the main libkrun workspace. The parent
`corresponding-source/Cargo.toml` provides an isolated archival workspace so
Cargo can inspect this snapshot without treating it as current tooling. Do not
edit the pinned source files when changing `src/libkrunfw-win`.

Git blob identities from that commit:

- `Cargo.toml`: `602af33a35dd67933ada726e5829bf4ba3a8a545`
- `build.rs`: `8ae91913ea4a7200680d8887fd75fe1b811ce647`
- `src/lib.rs`: `7b689f7b81d1b65eff70acf82a7951487fd0a4e4`

Raw file SHA-256 identities:

- `Cargo.toml`: `3d0fc32f8f7221e754e8e511176176c82706469ab273b969c44d874b71876d87`
- `build.rs`: `edfb76a021ce3e0e7694f74cbfe0c3424deaf4228bc28202c5a2e40711be94e5`
- `src/lib.rs`: `142b13f7a3b820461a12dcd70f67d96b59bb0246c87aba6967ad7f09a7f1f417`

The embedded kernel bundle is byte-identical to the official libkrunfw v5.5.0
x86_64 bundle. Its corresponding kernel source is Linux 6.12.91 plus the
`config-libkrunfw_x86_64` configuration and the 30-patch series from the
libkrunfw v5.5.0 source release. See the outer `a3s-libkrun-sys`
`SOURCE-PROVENANCE.md` for the immutable source and artifact hashes.
