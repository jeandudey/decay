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
`src/libs/glibc.zig`) to answer `has_function` for `linux`+`gnu`.

Tracked via git-lfs (see `.gitattributes`): it is a binary blob, not source,
and a repo clone that only builds other crates should not have to fetch it.

## Refreshing

Never hand-edit this file or add/remove a symbol by hand. To refresh it,
download a newer zig release's tarball, verify its shasum against
`https://ziglang.org/download/index.json`, extract
`lib/libc/glibc/abilists`, replace this file with it verbatim, and update the
release/checksums recorded above.
