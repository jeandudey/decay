//! Whether a symbol is part of glibc or musl, on `linux`, for any of
//! [`Cpu::ALL`], without hand-curating a list of function names — plus the
//! thin [`zig`] wrappers everything else in decay uses to ask a real
//! `zig cc` a yes/no question.
//!
//! `has_function('dlvsym')` and the like otherwise have to become an open
//! configuration knob, because a build graph cannot run the compiler. Both
//! halves here answer without that knob, and neither is a typed-out symbol
//! list:
//!
//! * **glibc** — `zig env`'s `lib_dir/libc/glibc/abilists`, read at
//!   runtime. That is the data glibc's own project publishes describing
//!   which symbols each released glibc version exports, for which target
//!   triples; zig ships a verbatim copy and uses it to synthesize glibc
//!   stub `.so`s for cross-linking. This module parses it directly (format
//!   mirrors `loadMetaData` and the function-inclusion loop in zig's
//!   `src/libs/glibc.zig`). Refreshing it means installing a newer zig,
//!   never editing a list.
//!
//! * **FreeBSD / NetBSD** — `lib_dir/libc/{freebsd,netbsd}/abilists`, the
//!   same byte-for-byte format, shipped for the same reason (zig
//!   cross-links a stub libc from it). Each BSD has exactly one libc and no
//!   abi split, so [`Libc::Freebsd`] / [`Libc::Netbsd`] double as the OS
//!   selector. NetBSD ships no `riscv64` column.
//!
//! * **musl** — no equivalent file exists (musl has no symbol versioning,
//!   so zig compiles it from source per target rather than shipping a
//!   stub-generation database). Instead this asks a real `zig cc`, the way
//!   meson asks a real compiler: which functions musl's own vendored
//!   headers declare for an arch, and which of those actually link. Done
//!   once per process, memoised, on first use — importing a project still
//!   never shells out for a symbol glibc's list already settles.

pub mod zig;

mod cpu;

pub use cpu::Cpu;

use std::{
    collections::{
        BTreeSet,
        HashMap,
        HashSet, //
    },
    path::{
        Path,
        PathBuf, //
    },
    sync::OnceLock,
};

/// Which libc [`has_function`] answers for. `Freebsd` / `Netbsd` also name
/// the OS — each ships one libc, with a bundled `abilists` just like glibc's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
    Glibc,
    Musl,
    Freebsd,
    Netbsd,
}

/// Whether `name` is a function symbol `libc` exports on `linux`, for `cpu`.
///
/// `false` only means "not found in this database" — it says nothing about
/// any other libc, and a caller must not treat it as proof the symbol does
/// not exist anywhere.
pub fn has_function(libc: Libc, cpu: Cpu, name: &str) -> bool {
    match libc {
        Libc::Glibc => glibc_table()
            .get(&cpu)
            .is_some_and(|col| col.functions.contains(name)),
        Libc::Musl => musl_table().get(&cpu).is_some_and(|col| {
            col.functions
                .binary_search_by(|s| s.as_str().cmp(name))
                .is_ok()
        }),
        Libc::Freebsd | Libc::Netbsd => bsd_table(libc)
            .get(&cpu)
            .is_some_and(|col| col.functions.contains(name)),
    }
}

/// Whether `cc.find_library('name')` resolves against `libc` on `linux`, for
/// `cpu` — i.e. whether `-lname` names a library that libc splits out (or
/// ships an empty compat stub for). Derived, not curated: glibc's half is
/// the abilist's own library table; musl's is a real `zig cc -lname` link.
///
/// `libc` itself (`c`) and the dynamic linker (`ld`) count as present. As
/// with [`has_function`], `false` only means "not known here".
pub fn has_library(libc: Libc, cpu: Cpu, name: &str) -> bool {
    if name == "c" {
        return true;
    }
    match libc {
        Libc::Glibc => glibc_table()
            .get(&cpu)
            .is_some_and(|col| col.libraries.contains(name)),
        Libc::Musl => musl_table().get(&cpu).is_some_and(|col| {
            col.libraries
                .binary_search_by(|s| s.as_str().cmp(name))
                .is_ok()
        }),
        // BSD `find_library()` is answered by a live `zig cc -target … -l`
        // link in `decay`'s `probe.rs`, not from here — the BSD abilists'
        // library tables only tag a name when some *function* symbol in this
        // column points at it, so libm (mostly value-returning math) can go
        // unlisted. Nothing calls this arm today.
        Libc::Freebsd | Libc::Netbsd => false,
    }
}

// ---------------------------------------------------------------------------
// glibc / BSD: parse `abilists` from the zig install (one binary format)
// ---------------------------------------------------------------------------

/// One `<arch>-<os>-<abi>` column of an abilist: the function symbols it
/// exports, and the library names those symbols live in (`m`, `pthread`,
/// `dl`, `rt`, `util`, `resolv`, `c`, `ld` — whatever the abilist's own
/// library table happens to list).
struct AbiColumn {
    functions: HashSet<&'static str>,
    libraries: HashSet<&'static str>,
}

/// The `abilists` bytes shipped under `zig env`'s `lib_dir/libc/<subdir>`,
/// read and leaked to `'static` so the zero-copy parser below can hand back
/// `&'static str` slices into them. ~45–250 KiB, once per libc per process.
fn abilists(subdir: &str) -> &'static [u8] {
    let path = zig::lib_dir().join("libc").join(subdir).join("abilists");
    let data = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "could not read {subdir} abilists from the zig install at {}: {e}",
            path.display()
        )
    });
    &*Box::leak(data.into_boxed_slice())
}

fn glibc_table() -> &'static HashMap<Cpu, AbiColumn> {
    static TABLE: OnceLock<HashMap<Cpu, AbiColumn>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let data = abilists("glibc");
        Cpu::ALL
            .into_iter()
            .map(|cpu| {
                let column =
                    parse(data, cpu.zig_arch(), "linux", cpu.glibc_abi()).unwrap_or_else(|| {
                        panic!(
                            "the zig install's glibc abilists is malformed, or has no \
                             {}-linux-{} column",
                            cpu.zig_arch(),
                            cpu.glibc_abi()
                        )
                    });
                (cpu, column)
            })
            .collect()
    })
}

/// The parsed table for a BSD's bundled `abilists`, one column per [`Cpu`]
/// zig ships data for. `libc` must be `Freebsd` or `Netbsd`. NetBSD has no
/// `riscv64` column — that [`Cpu`] is simply absent from the map, which
/// [`has_function`] / [`has_library`] read as a settled not-found.
fn bsd_table(libc: Libc) -> &'static HashMap<Cpu, AbiColumn> {
    static FREEBSD: OnceLock<HashMap<Cpu, AbiColumn>> = OnceLock::new();
    static NETBSD: OnceLock<HashMap<Cpu, AbiColumn>> = OnceLock::new();
    let (cell, os) = match libc {
        Libc::Freebsd => (&FREEBSD, "freebsd"),
        Libc::Netbsd => (&NETBSD, "netbsd"),
        other => unreachable!("bsd_table called with {other:?}"),
    };
    cell.get_or_init(|| {
        let data = abilists(os);
        Cpu::ALL
            .into_iter()
            .filter_map(|cpu| Some((cpu, parse(data, cpu.zig_arch(), os, cpu.bsd_abi())?)))
            .collect()
    })
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

/// Parses the abilist into the set of function symbol names present for the
/// `<arch>-<os>-<abi>` triple (e.g. `x86_64`, `linux`, `gnu`).
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
fn parse(data: &'static [u8], arch: &str, os: &str, abi: &str) -> Option<AbiColumn> {
    let mut idx = 0usize;

    let n_libs = *data.get(idx)? as usize;
    idx += 1;
    let lib_names: Vec<&'static str> = (0..n_libs)
        .map(|_| read_cstr(data, &mut idx))
        .collect::<Option<_>>()?;

    let n_versions = *data.get(idx)? as usize;
    idx += 1;
    idx += n_versions * 3;

    let n_targets = *data.get(idx)? as usize;
    idx += 1;
    let mut target_index = None;
    for i in 0..n_targets {
        let triple = read_cstr(data, &mut idx)?;
        let mut parts = triple.split('-');
        let (t_arch, t_os, t_abi) = (parts.next()?, parts.next()?, parts.next()?);
        if t_os == os && t_arch == arch && t_abi == abi {
            target_index = Some(i);
        }
    }
    let target_index = target_index? as u32;

    let mut functions = HashSet::new();
    let mut libraries = HashSet::new();

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
            functions.insert(name);
            // The low 7 bits are this inclusion's index into the library
            // table; a symbol present for this target means its library is
            // too (`-lFOO` resolves).
            if let Some(lib) = lib_names.get((lib_byte & 0x7f) as usize) {
                libraries.insert(*lib);
            }
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

    Some(AbiColumn {
        functions,
        libraries,
    })
}

// ---------------------------------------------------------------------------
// musl: ask a real `zig cc`, once per process
// ---------------------------------------------------------------------------

/// One arch's musl answer: the function symbols that both musl's headers
/// declare and a real link resolves, and the `-lNAME` names that link. Both
/// sorted, for `binary_search`.
struct MuslColumn {
    functions: Vec<String>,
    libraries: Vec<String>,
}

/// The `-lFOO` names a libc conventionally splits out (or keeps an empty
/// compat archive for). Not the answer — just the question set handed to
/// the linker below, the same way [`declared_functions`] feeds
/// [`link_present`].
///
/// A name musl neither splits nor stubs simply fails to resolve and drops
/// out. Extend this if a project asks `find_library()` for something else
/// libc-provided.
const SPLIT_LIB_CANDIDATES: &[&str] = &[
    "m", "pthread", "dl", "rt", "util", "resolv", "crypt", "xnet", "nsl", "anl", "execinfo",
];

fn musl_table() -> &'static HashMap<Cpu, MuslColumn> {
    static TABLE: OnceLock<HashMap<Cpu, MuslColumn>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let found = zig::version();
        if found != zig::EXPECTED_VERSION {
            eprintln!(
                "decay: zig {found} on PATH; decay's musl has_function answers were \
                 developed against {}. musl has no symbol versioning, so a difference \
                 is unlikely, but this is where one would come from.",
                zig::EXPECTED_VERSION
            );
        }

        let lib_dir = zig::lib_dir();
        let header_root = lib_dir.join("libc").join("include").join("generic-musl");
        let headers = find_headers(&header_root);
        assert!(
            !headers.is_empty(),
            "found no musl headers under {}; is `zig env`'s lib_dir right?",
            header_root.display(),
        );

        Cpu::ALL
            .into_iter()
            .map(|cpu| {
                let candidates = declared_functions(cpu, &headers);
                let mut functions: Vec<String> =
                    link_present(cpu, &candidates).into_iter().collect();
                functions.sort();
                let libraries = library_link_probe(cpu);
                (
                    cpu,
                    MuslColumn {
                        functions,
                        libraries,
                    },
                )
            })
            .collect()
    })
}

/// Every `.h` file under `root`, recursing into subdirectories except any
/// named `bits` — musl's own convention (shared with glibc) for
/// "implementation detail, not meant to be `#include`d on its own", already
/// pulled in by whichever public header needs it.
fn find_headers(root: &Path) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "bits") {
                    continue;
                }
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "h") {
                out.push(path);
            }
        }
    }
    let mut headers = Vec::new();
    visit(root, &mut headers);
    headers.sort();
    headers
}

/// Every function name musl's own headers declare for `cpu`, per real
/// parsing by zig's own `clang` frontend rather than a hand-rolled header
/// scanner. Run once per arch (not once, reused): the header text is the
/// same, but what clang's preprocessor keeps from it depends on that arch's
/// predefined macros.
fn declared_functions(cpu: Cpu, headers: &[PathBuf]) -> BTreeSet<String> {
    // Without `_ALL_SOURCE`, musl's `features.h` hides every GNU/BSD
    // extension declaration behind a feature-test guard, and this would
    // only ever see plain POSIX names.
    let mut src = String::from("#define _ALL_SOURCE 1\n");
    for header in headers {
        src.push_str(&format!("#include \"{}\"\n", header.display()));
    }

    let ast = zig::ast_dump(&src, &cpu.musl_target());
    ast.lines()
        .filter(|line| line.contains("FunctionDecl"))
        .filter_map(|line| {
            // A `FunctionDecl` node's name is always the identifier right
            // before its quoted type, e.g. `...col:7 implicit memcpy 'void
            // *(void *, const void *, unsigned long)' extern`.
            let name = line
                .split('\'')
                .next()?
                .trim_end()
                .rsplit(char::is_whitespace)
                .next()?;
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

/// Which of `candidates` a real `zig cc` link for `cpu` resolves, by
/// declaring all of them `extern` in one C file, taking each one's address
/// (forcing a real reference, not a compiler builtin substitution), and
/// reading the linker's "undefined symbol" diagnostics back out — one link
/// for every candidate at once, rather than one per name.
fn link_present(cpu: Cpu, candidates: &BTreeSet<String>) -> BTreeSet<String> {
    let mut src = String::new();
    for name in candidates {
        src.push_str(&format!("extern int {name}();\n"));
    }
    src.push_str("void *_decay_zig_refs[] = {\n");
    for name in candidates {
        src.push_str(&format!("    (void*)&{name},\n"));
    }
    src.push_str("};\nint main(void) { return 0; }\n");

    let target = cpu.musl_target();
    match zig::link(&src, &target, &[]) {
        // Every candidate resolved — unlikely, but not an error.
        Ok(()) => candidates.clone(),
        Err(stderr) => {
            // Expected: most candidates are declared for some other target
            // or configuration and are reported undefined here. A failure
            // with *no* "undefined symbol" lines means the probe failed to
            // build for an unrelated reason instead of linking — which
            // would otherwise read as "musl exports every candidate",
            // silently and wrongly. Catch that rather than trust an empty
            // diff.
            let missing: BTreeSet<String> = stderr
                .lines()
                .filter_map(|line| {
                    line.trim()
                        .strip_prefix("ld.lld: error: undefined symbol: ")
                })
                .map(str::to_owned)
                .collect();
            assert!(
                !missing.is_empty(),
                "zig cc -target {target} reported no undefined symbols at all out of {} \
                 candidates — the probe likely failed to build for an unrelated reason; \
                 zig's output was:\n{stderr}",
                candidates.len(),
            );
            candidates.difference(&missing).cloned().collect()
        }
    }
}

/// Which of [`SPLIT_LIB_CANDIDATES`] a real `zig cc -target <arch>-linux-musl`
/// resolves `-lNAME` for, sorted.
fn library_link_probe(cpu: Cpu) -> Vec<String> {
    let target = cpu.musl_target();
    let mut found: Vec<String> = SPLIT_LIB_CANDIDATES
        .iter()
        .filter(|name| zig::link("int main(void) { return 0; }\n", &target, &[name]).is_ok())
        .map(|name| (*name).to_owned())
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::{Cpu, Libc, has_function, has_library};

    #[test]
    fn crt_split_libraries_resolve_on_every_arch() {
        // The libraries glibc splits out and musl keeps empty compat
        // archives for; `cc.find_library()` for any of them resolves.
        for cpu in Cpu::ALL {
            for name in ["m", "dl", "rt", "pthread", "resolv", "util", "c"] {
                assert!(has_library(Libc::Glibc, cpu, name), "glibc {cpu:?}: {name}");
                assert!(has_library(Libc::Musl, cpu, name), "musl {cpu:?}: {name}");
            }
        }
    }

    #[test]
    fn does_not_invent_libraries() {
        for cpu in Cpu::ALL {
            assert!(!has_library(Libc::Glibc, cpu, "definitely_not_a_library"));
            assert!(!has_library(Libc::Musl, cpu, "definitely_not_a_library"));
            // glibc has never shipped a standalone libsocket (that is Solaris).
            assert!(!has_library(Libc::Glibc, cpu, "socket"));
        }
    }

    #[test]
    fn finds_a_gnu_extension_on_every_arch() {
        // A GNU extension no other libc ships, and the motivating example
        // for this database (see `example/decay.toml`'s hand-written
        // `has_function:dlvsym` entry, which this makes automatic).
        for cpu in Cpu::ALL {
            assert!(has_function(Libc::Glibc, cpu, "dlvsym"), "{cpu:?}");
            assert!(!has_function(Libc::Musl, cpu, "dlvsym"), "{cpu:?}");
        }
    }

    #[test]
    fn finds_plain_libc_on_every_arch() {
        for cpu in Cpu::ALL {
            for name in ["malloc", "strlen", "printf"] {
                assert!(
                    has_function(Libc::Glibc, cpu, name),
                    "glibc {cpu:?}: {name}"
                );
                assert!(has_function(Libc::Musl, cpu, name), "musl {cpu:?}: {name}");
            }
        }
    }

    #[test]
    fn finds_a_musl_capable_extension() {
        // Declared and implemented by musl, not by glibc's abilist for
        // whatever glibc version this was pinned against.
        assert!(has_function(Libc::Musl, Cpu::X86_64, "explicit_bzero"));
    }

    #[test]
    fn does_not_find_nonsense() {
        for cpu in Cpu::ALL {
            assert!(!has_function(
                Libc::Glibc,
                cpu,
                "this_is_not_a_real_libc_symbol"
            ));
            assert!(!has_function(
                Libc::Musl,
                cpu,
                "this_is_not_a_real_libc_symbol"
            ));
        }
    }

    #[test]
    fn does_not_find_a_static_inline_only_declaration() {
        // Declared in musl's headers but implemented as `static __inline`
        // (byteswap.h) — no linkable symbol, so this must stay absent even
        // though step 1 of the musl pass sees the declaration.
        assert!(!has_function(Libc::Musl, Cpu::X86_64, "__bswap_32"));
    }

    #[test]
    fn finds_bsd_libc_and_a_bsd_only_api() {
        for libc in [Libc::Freebsd, Libc::Netbsd] {
            for cpu in [Cpu::X86_64, Cpu::Arm64, Cpu::Arm32] {
                for name in ["malloc", "printf", "strlen"] {
                    assert!(has_function(libc, cpu, name), "{libc:?} {cpu:?}: {name}");
                }
            }
            // kqueue is the BSD event API glibc has never shipped; epoll is
            // Linux-only and must stay absent here.
            assert!(
                has_function(libc, Cpu::X86_64, "kqueue"),
                "{libc:?}: kqueue"
            );
            assert!(
                !has_function(libc, Cpu::X86_64, "epoll_create1"),
                "{libc:?}"
            );
            assert!(!has_function(
                libc,
                Cpu::X86_64,
                "this_is_not_a_real_libc_symbol"
            ));
        }
        assert!(!has_function(Libc::Glibc, Cpu::X86_64, "kqueue"));
    }

    #[test]
    fn netbsd_ships_no_riscv64_column() {
        // A settled not-found, not a panic — zig has no netbsd/riscv64 data.
        assert!(!has_function(Libc::Netbsd, Cpu::Riscv64, "malloc"));
        // FreeBSD does carry riscv64.
        assert!(has_function(Libc::Freebsd, Cpu::Riscv64, "malloc"));
    }

    #[test]
    fn finds_an_x86_only_musl_function_on_x86_but_not_arm() {
        // x86 I/O-port privilege syscalls: declared in musl's headers on
        // every arch, but only ever linkable on x86(_64) — the motivating
        // example for probing every arch separately rather than reusing one
        // arch's answer everywhere.
        assert!(has_function(Libc::Musl, Cpu::X86_64, "ioperm"));
        assert!(!has_function(Libc::Musl, Cpu::Arm64, "ioperm"));
    }
}
