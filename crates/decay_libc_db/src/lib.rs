//! Whether a symbol is part of glibc, on `linux`+`gnu`, without hand-curating
//! a list of function names.
//!
//! `has_function('dlvsym')` and the like otherwise have to become an open
//! configuration knob, because the importer cannot run the compiler. For a
//! symbol glibc itself exports, the answer is knowable without running
//! anything: `data/glibc-abilists` is copied verbatim from a pinned zig
//! release (see `data/PROVENANCE.md`) — the same data glibc's own project
//! publishes, and the data zig uses to synthesize glibc stubs for
//! cross-linking. This module parses that file directly; nothing here is a
//! hand-typed symbol list, and refreshing it means copying in a newer
//! release's copy, never editing one.
//!
//! The binary format below mirrors `loadMetaData` and the function-inclusion
//! loop in zig's own `src/libs/glibc.zig`.

use std::{
    collections::HashSet,
    sync::OnceLock, //
};

/// The vendored `abilists` file. See `data/PROVENANCE.md`.
static ABILISTS: &[u8] = include_bytes!("../data/glibc-abilists");

/// The triple this database answers for. A later pass can read other columns
/// of the same file for other architectures; scoped to one for now.
const TARGET_ARCH: &str = "x86_64";
const TARGET_ABI: &str = "gnu";

fn symbols() -> &'static HashSet<&'static str> {
    static SYMBOLS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SYMBOLS.get_or_init(|| {
        parse(ABILISTS).expect("crates/decay_libc_db/data/glibc-abilists is malformed")
    })
}

/// Whether `name` is a glibc function symbol on `x86_64-linux-gnu`.
///
/// `false` only means "not found in this database" — it says nothing about
/// musl, macOS, Windows, or any other libc, and a caller must not treat it as
/// proof the symbol does not exist anywhere.
pub fn has_function(name: &str) -> bool {
    symbols().contains(name)
}

/// Reads a NUL-terminated string starting at `*idx`, advancing past the NUL.
fn read_cstr(data: &'static [u8], idx: &mut usize) -> Option<&'static str> {
    let start = *idx;
    let end = start + data[start..].iter().position(|&b| b == 0)?;
    *idx = end + 1;
    std::str::from_utf8(&data[start..end]).ok()
}

/// Reads a ULEB128-encoded `u64` starting at `*idx`.
fn read_uleb128_u64(data: &[u8], idx: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *data.get(*idx)?;
        *idx += 1;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
}

/// Parses the vendored abilist into the set of function symbol names present
/// for [`TARGET_ARCH`]-linux-[`TARGET_ABI`].
///
/// Format, verbatim from zig's `loadMetaData` (the header) and the
/// function-inclusion loop in `buildSharedObjects` (the body):
///
/// ```text
/// u8               lib count
/// (cstr)*          that many library names ("m", "pthread", "c", ...)
/// u8               version count
/// (u8 u8 u8)*      that many (major, minor, patch) triples
/// u8               target count
/// (cstr)*          that many "<arch>-linux-<abi>" triples
/// u16 (LE)         function-inclusion count
/// inclusion*       that many function inclusions (below)
/// u16 (LE)         object-inclusion count (data symbols; not parsed here)
/// ```
///
/// An inclusion is one (symbol name, targets, library, versions) fact; a
/// symbol with entries in more than one library or target list appears as
/// several consecutive inclusions that share one name, the last one marked
/// terminal:
///
/// ```text
/// (cstr)?          the symbol name, only when the previous inclusion for
///                  this symbol was not terminal (i.e. omitted otherwise)
/// uleb128          bitset of which target index this inclusion covers
/// u8               library index; high bit set means this is the last
///                  inclusion for this symbol name
/// ver+             one byte per version this inclusion holds for; high bit
///                  set on the last one
/// ```
fn parse(data: &'static [u8]) -> Option<HashSet<&'static str>> {
    let mut idx = 0usize;

    let n_libs = *data.get(idx)? as usize;
    idx += 1;
    for _ in 0..n_libs {
        read_cstr(data, &mut idx)?;
    }

    let n_versions = *data.get(idx)? as usize;
    idx += 1;
    idx += n_versions * 3;

    let n_targets = *data.get(idx)? as usize;
    idx += 1;
    let mut target_index = None;
    for i in 0..n_targets {
        let triple = read_cstr(data, &mut idx)?;
        let mut parts = triple.split('-');
        let (arch, os, abi) = (parts.next()?, parts.next()?, parts.next()?);
        if os == "linux" && arch == TARGET_ARCH && abi == TARGET_ABI {
            target_index = Some(i);
        }
    }
    let target_index = target_index? as u32;

    let mut present = HashSet::new();

    let fn_inclusions_len = u16::from_le_bytes([*data.get(idx)?, *data.get(idx + 1)?]) as usize;
    idx += 2;

    let mut pending_name: Option<&'static str> = None;
    for _ in 0..fn_inclusions_len {
        let name = match pending_name {
            Some(name) => name,
            None => read_cstr(data, &mut idx)?,
        };

        let targets = read_uleb128_u64(data, &mut idx)?;
        let lib_byte = *data.get(idx)?;
        idx += 1;
        pending_name = if lib_byte & 0x80 != 0 {
            None
        } else {
            Some(name)
        };

        if targets & (1u64 << target_index) != 0 {
            present.insert(name);
        }

        // The version-index run for this inclusion; its content does not
        // matter here, only walking past it to reach the next inclusion.
        loop {
            let byte = *data.get(idx)?;
            idx += 1;
            if byte & 0x80 != 0 {
                break;
            }
        }
    }

    Some(present)
}

#[cfg(test)]
mod tests {
    use super::has_function;

    #[test]
    fn finds_a_gnu_extension() {
        // A GNU extension no other libc ships, and the motivating example
        // for this database (see `example/decay.toml`'s hand-written
        // `has_function:dlvsym` entry, which this makes automatic).
        assert!(has_function("dlvsym"));
    }

    #[test]
    fn finds_plain_libc() {
        assert!(has_function("malloc"));
        assert!(has_function("strlen"));
        assert!(has_function("printf"));
    }

    #[test]
    fn does_not_find_nonsense() {
        assert!(!has_function("this_is_not_a_real_libc_symbol"));
    }
}
