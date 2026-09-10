//! Answering `cc.has_header` / `cc.has_type` / `cc.compiles` /
//! `cc.find_library` by actually building the probe with `zig`, once per
//! target in decay's configured matrix, instead of leaving each one an open
//! knob.
//!
//! `zig` is a hard requirement of decay (see [`decay_zig`]); this module
//! adds only the matrix iteration, the meson-flag filtering, and the
//! per-run cache on top of [`decay_zig::zig`]'s bare `compiles` / `link`.

use {
    decay_meson_eval::oracle::CompileProbe,
    decay_zig::{
        Cpu,
        zig, //
    },
    std::collections::HashMap,
};

/// The buck2 constraint settings a [`CompileProbe`] answer selects on.
pub const ABI_SETTING: &str = "prelude//abi/constraints:abi";
pub const CPU_SETTING: &str = "prelude//cpu/constraints:cpu";

/// `(target triple, source) -> did it compile`, so a header asked about in
/// twenty files is built once per target, not twenty times.
///
/// ponytail: unbounded and never evicted — one process imports a bounded
/// set of projects, so it stays small; add an LRU if that stops being true.
#[derive(Default)]
pub struct ProbeCache(HashMap<(String, String), bool>);

impl ProbeCache {
    fn compiles(&mut self, triple: &str, snippet: &str, extra: &[&str]) -> bool {
        let key = (
            triple.to_owned(),
            format!("{snippet}\u{0}{}", extra.join(" ")),
        );
        if let Some(hit) = self.0.get(&key) {
            return *hit;
        }
        let ok = zig::compiles(snippet, triple, extra);
        self.0.insert(key, ok);
        ok
    }

    /// Whether `zig cc -target <triple> … -l<lib>` links — i.e. whether
    /// `cc.find_library('lib')` resolves for that target. Cached in the same
    /// map; the `-l` prefix keeps the key space disjoint from `compiles`.
    ///
    /// A `-target` zig cannot host (no bundled libc/sysroot: illumos,
    /// openbsd, android, fuchsia) fails the link for reasons unrelated to
    /// the library, so a caller must only treat `true` as meaningful and
    /// get `false` from elsewhere.
    pub fn links_library(&mut self, triple: &str, lib: &str) -> bool {
        let key = (triple.to_owned(), format!("-l{lib}"));
        if let Some(hit) = self.0.get(&key) {
            return *hit;
        }
        let ok = zig::link("int main(void) { return 0; }\n", triple, &[lib]).is_ok();
        self.0.insert(key, ok);
        ok
    }
}

/// Each configured system decay can link-probe with `zig cc`, paired with
/// the `(abi buck2 value, zig -target triple)` list to try for it. `""` abi
/// means "every abi on this system". A system not here — `illumos`,
/// `openbsd`, `android`, `fuchsia` — has no bundled zig libc and must be
/// answered from `decay.toml` instead. `windows` probes only `gnu` (zig has
/// no MSVC libc); the caller decides whether a `gnu` hit also covers
/// `abi[msvc]`.
pub fn system_link_targets(system: &str) -> Option<Vec<(&'static str, &'static str)>> {
    let targets = match system {
        "linux" => vec![("", "x86_64-linux-gnu"), ("", "x86_64-linux-musl")],
        "darwin" => vec![("", "aarch64-macos"), ("", "x86_64-macos")],
        "freebsd" => vec![("", "x86_64-freebsd")],
        "netbsd" => vec![("", "x86_64-netbsd")],
        "windows" => vec![("gnu", "x86_64-windows-gnu")],
        _ => return None,
    };
    Some(targets)
}

/// Operating systems decay compile-probes with `zig cc`: the ones whose
/// libc headers zig bundles in full, so a header that does not `#include`
/// there is reliably *absent* rather than just missing from a partial SDK.
/// `darwin` / `windows` do not qualify (zig ships a macOS-SDK / mingw
/// subset), so a compile probe there stays an open knob.
pub const PROBE_SYSTEMS: [&str; 3] = ["linux", "freebsd", "netbsd"];

/// GCC version `zig cc` (clang) is told to report through `__GNUC__` &c.
/// decay's emitted builds compile with gcc, so a probe gated on
/// `__GNUC__ >= N` should answer as gcc would, not as clang's default
/// spoofed `4.2.1` (which fails graphene's `>= 4.9` vector check and its
/// kind).
///
/// ponytail: ceiling is real — `11`+ makes glibc's `sys/cdefs.h` use the
/// two-argument `__malloc__(dealloc, n)` attribute, which zig's clang does
/// not implement, so every glibc header stops compiling. `10.x` is the
/// highest that keeps glibc parsing. Make it configurable if a project must
/// probe against a specific newer gcc.
const GNUC_VERSION: &str = "-fgnuc-version=10.5.0";

/// The `(abi buck2 value, zig `-target` triple)` pairs decay probes for a
/// CPU on `system`. `""` abi means the system has no glibc/musl-style split.
/// Mirrors `decay_zig`'s own arch/abi spelling (arm32 is hard-float EABI
/// either way).
fn probe_targets(system: &str, cpu: Cpu) -> Vec<(&'static str, String)> {
    match system {
        "linux" => vec![
            (
                "gnu",
                format!("{}-linux-{}", cpu.zig_arch(), cpu.glibc_abi()),
            ),
            ("musl", cpu.musl_target()),
        ],
        "freebsd" => vec![("", format!("{}-freebsd", cpu.zig_arch()))],
        "netbsd" => vec![("", format!("{}-netbsd", cpu.zig_arch()))],
        _ => Vec::new(),
    }
}

/// Build `probe`'s snippet for every `system` target in the matrix, replaying
/// the probe's `args:` (already vetted as plain flags), and return the rows
/// (buck2 constraint values) it compiled for — `[abi, cpu]` on a system with
/// an abi split, `[cpu]` otherwise.
pub fn probe_rows(cache: &mut ProbeCache, probe: &CompileProbe, system: &str) -> Vec<Vec<String>> {
    let snippet = probe.snippet();
    let mut rows = Vec::new();
    for cpu in Cpu::ALL {
        let arch = match cpu.zig_arch() {
            "x86_64" | "x86" => "x86",
            other => other,
        };
        let mut flags = vec![GNUC_VERSION];
        flags.extend(flags_for_arch(probe.args(), arch));
        for (abi, triple) in probe_targets(system, cpu) {
            if cache.compiles(&triple, &snippet, &flags) {
                rows.push(match abi {
                    "" => vec![cpu.buck2_value().to_owned()],
                    _ => vec![abi.to_owned(), cpu.buck2_value().to_owned()],
                });
            }
        }
    }
    rows
}

/// The subset of `flags` valid for `arch`: an ISA `-m…` option handed to a
/// `-target` that lacks that ISA makes `zig cc` (clang) hard-error, so drop
/// the two families that do — ARM `-mfpu=`/`-mfloat-abi=` off non-ARM, x86
/// `-msse*`/`-mavx*`/`-mfpmath=sse` off non-x86. Everything else (`-D…`,
/// `-std=…`, `-f…`, `-W…`) passes through untouched.
///
/// ponytail: two hard-coded ISA families; extend if a project brings a
/// `-target`-incompatible `-m…` from another arch.
fn flags_for_arch<'a>(flags: &'a [String], arch: &str) -> Vec<&'a str> {
    flags
        .iter()
        .map(String::as_str)
        .filter(|f| {
            let arm_isa = f.starts_with("-mfpu=") || f.starts_with("-mfloat-abi=");
            let x86_isa = f.starts_with("-msse")
                || f.starts_with("-mavx")
                || *f == "-mmmx"
                || *f == "-mfpmath=sse";
            match arch {
                "arm" => !x86_isa,
                "x86" => !arm_isa,
                _ => !arm_isa && !x86_isa,
            }
        })
        .collect()
}

/// Whether a `has_header` argument is a plain header path, safe to drop into
/// `#include <...>`. Anything stranger is left open rather than guessed at.
pub fn is_plain_header(header: &str) -> bool {
    !header.is_empty()
        && header
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/+".contains(&b))
}

#[cfg(test)]
mod tests {
    use {super::*, decay_meson_eval::oracle::CompileProbeKind};

    fn probe(kind: CompileProbeKind) -> CompileProbe {
        CompileProbe {
            kind,
            args: Vec::new(),
        }
    }

    #[test]
    fn header_rows_match_reality() {
        let mut cache = ProbeCache::default();

        let present = probe_rows(
            &mut cache,
            &probe(CompileProbeKind::Header {
                header: "stdio.h".to_owned(),
            }),
            "linux",
        );
        assert_eq!(
            present.len(),
            Cpu::ALL.len() * 2,
            "stdio.h should compile for every linux target"
        );

        let absent = probe_rows(
            &mut cache,
            &probe(CompileProbeKind::Header {
                header: "decay_no_such_header_xyz.h".to_owned(),
            }),
            "linux",
        );
        assert!(absent.is_empty(), "a bogus header compiles nowhere");
    }

    #[test]
    fn arch_isa_flags_gate_by_target() {
        // ARM `-mfpu=` only ever reaches an arm32 target.
        let neon = vec!["-mfpu=neon".to_owned(), "-D_GNU_SOURCE".to_owned()];
        assert_eq!(
            flags_for_arch(&neon, "arm"),
            ["-mfpu=neon", "-D_GNU_SOURCE"]
        );
        assert_eq!(flags_for_arch(&neon, "x86"), ["-D_GNU_SOURCE"]);
        assert_eq!(flags_for_arch(&neon, "aarch64"), ["-D_GNU_SOURCE"]);

        // x86 `-msse*` only reaches an x86 target.
        let sse = vec!["-mfpmath=sse".to_owned(), "-msse2".to_owned()];
        assert_eq!(flags_for_arch(&sse, "x86"), ["-mfpmath=sse", "-msse2"]);
        assert!(flags_for_arch(&sse, "arm").is_empty());
        assert!(flags_for_arch(&sse, "riscv64").is_empty());
    }

    #[test]
    fn args_replay_flips_a_row() {
        let mut cache = ProbeCache::default();

        // graphene's NEON probe: #errors unless __ARM_NEON__ (set by
        // -mfpu=neon on arm32) or __aarch64__. So: arm32 + arm64 only.
        let neon_prog = "\
#if !defined (__ARM_NEON__) && !defined (__aarch64__)
# error no neon
#endif
#include <arm_neon.h>
int main (void) { return 0; }
";
        let rows = probe_rows(
            &mut cache,
            &CompileProbe {
                kind: CompileProbeKind::Compiles {
                    prefix: String::new(),
                    code: neon_prog.to_owned(),
                },
                args: vec!["-mfpu=neon".to_owned()],
            },
            "linux",
        );
        let cpus: std::collections::BTreeSet<_> = rows.iter().map(|r| r[1].clone()).collect();
        assert_eq!(
            cpus,
            ["arm32", "arm64"].map(str::to_owned).into_iter().collect(),
            "NEON prog with -mfpu=neon compiles for arm only, not x86_64"
        );
    }

    #[test]
    fn link_probe_matches_reality() {
        let mut cache = ProbeCache::default();

        // libm resolves on every host zig can link for.
        for (_abi, triple) in system_link_targets("linux").unwrap() {
            assert!(cache.links_library(triple, "m"), "{triple} -lm");
        }
        assert!(cache.links_library("x86_64-macos", "m"));

        // libdl does not exist on mingw or netbsd.
        assert!(!cache.links_library("x86_64-windows-gnu", "dl"));
        assert!(!cache.links_library("x86_64-netbsd", "dl"));

        // A name that is not a library resolves nowhere.
        assert!(!cache.links_library("x86_64-linux-gnu", "decay_not_a_library_xyz"));

        // zig has no libc for these, so system_link_targets declines them.
        for system in ["sunos", "openbsd", "android", "fuchsia"] {
            assert!(system_link_targets(system).is_none(), "{system}");
        }
    }

    #[test]
    fn type_probe_respects_its_prefix() {
        let mut cache = ProbeCache::default();

        // No prefix: `struct iovec` is undeclared, compiles nowhere.
        let bare = probe_rows(
            &mut cache,
            &probe(CompileProbeKind::Type {
                name: "struct iovec".to_owned(),
                prefix: String::new(),
            }),
            "linux",
        );
        assert!(bare.is_empty());

        // With the header that declares it: compiles everywhere.
        let with_prefix = probe_rows(
            &mut cache,
            &probe(CompileProbeKind::Type {
                name: "struct iovec".to_owned(),
                prefix: "#include <sys/uio.h>".to_owned(),
            }),
            "linux",
        );
        assert_eq!(with_prefix.len(), Cpu::ALL.len() * 2);
    }

    #[test]
    fn bsd_rows_have_no_abi_axis() {
        let mut cache = ProbeCache::default();

        // A BSD row is just `[cpu]` — no glibc/musl split.
        let rows = probe_rows(
            &mut cache,
            &probe(CompileProbeKind::Header {
                header: "stdio.h".to_owned(),
            }),
            "freebsd",
        );
        assert!(!rows.is_empty(), "stdio.h compiles on freebsd");
        assert!(rows.iter().all(|r| r.len() == 1), "no abi axis on freebsd");
        assert!(rows.iter().any(|r| r[0] == "x86_64"));

        // `sys/epoll.h` is Linux-only — absent on the BSDs.
        let absent = probe_rows(
            &mut cache,
            &probe(CompileProbeKind::Header {
                header: "sys/epoll.h".to_owned(),
            }),
            "netbsd",
        );
        assert!(absent.is_empty(), "no epoll on netbsd");
    }
}
