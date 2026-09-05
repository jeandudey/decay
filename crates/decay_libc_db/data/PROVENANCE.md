# `glibc-abilists`

Copied verbatim, byte for byte, from:

```
lib/libc/glibc/abilists
```

inside the official zig release tarball:

- Release: `0.15.2`
- Tarball: <https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz>
- Tarball SHA-256: `02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239`
  (matches the `shasum` published at <https://ziglang.org/download/index.json>)
- `glibc-abilists` SHA-256: `ecc95d61f0020ad29d6771cddc856e28ac9fdbdc57526a0ba75b1e1197460c9a`

This is the data glibc's own project publishes describing which symbols each
released glibc version exports, for which target triples — the same data zig
uses to synthesize its glibc stub `.so`s for cross-linking without needing a
real glibc installed. `decay_libc_db` parses it directly (format documented by
`loadMetaData` and the function-inclusion loop in zig's
`src/libs/glibc.zig`) to answer `has_function` for `linux`+`gnu`, on every
architecture in `src/cpu.rs`'s `Cpu::ALL`.

Tracked via git-lfs (see `.gitattributes`): it is a binary blob, not source,
and a repo clone that only builds other crates should not have to fetch it.

## Refreshing

Never hand-edit this file or add/remove a symbol by hand. To refresh it,
download a newer zig release's tarball, verify its shasum against
`https://ziglang.org/download/index.json`, extract
`lib/libc/glibc/abilists`, replace this file with it verbatim, and update the
release/checksums recorded above.

## musl

musl has no equivalent file to vendor — it carries no symbol versioning, so
zig just compiles it from source per target rather than shipping a
stub-generation database the way it does for glibc. `../build.rs` derives
`has_function`'s musl half itself, at `decay_libc_db`'s own build time, for
every architecture in `Cpu::ALL`, by asking a real `zig cc` which functions
musl's own vendored headers declare and which of those actually link for
that arch (see `build.rs`'s doc comment) — nothing from this file, or from
glibc at all, feeds that half.

`Cpu::ALL` (`../src/cpu.rs`) is the intersection of buck2's
`prelude//cpu/constraints:cpu` values and the zig `linux` targets decay's own
`linux`-system gating can actually reach — five architectures today
(`x86_64`, `x86_32`, `arm64`, `arm32`, `riscv64`); see that file's own doc
comment for the two buck2 values that drop out of the intersection and why.

That means *building* `decay_libc_db` needs `zig` installed on `PATH` (this
crate was developed against the same `0.15.2` release pinned above; `build.rs`
warns, but does not fail, if a different one is found — musl's own ABI
stability makes a different answer unlikely).
