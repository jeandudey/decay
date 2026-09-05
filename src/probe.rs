//! Answering `cc.has_header` / `cc.has_type` / `cc.compiles` by actually
//! compiling the probe, once per target in decay's configured matrix,
//! instead of leaving each one an open knob.
//!
//! This is the one place decay shells out to a compiler at import time.
//! `zig` is a single hermetic cross-compiler with bundled headers for every
//! target, so one binary covers the whole matrix without sysroots. Absent
//! from `PATH`, every probe here declines and stays open, exactly as
//! before — see `decay_libc_db/data/PROVENANCE.md` for the release the
//! `has_function` database was pinned against.

use {
    decay_libc_db::Cpu,
    decay_meson_eval::oracle::CompileProbe,
    std::{
        collections::HashMap,
        fs,
        process::{
            Command,
            Stdio, //
        },
        sync::{
            OnceLock,
            atomic::{
                AtomicU64,
                Ordering, //
            },
        },
    },
};

/// The buck2 constraint settings a [`CompileProbe`] answer selects on.
pub const ABI_SETTING: &str = "prelude//abi/constraints:abi";
pub const CPU_SETTING: &str = "prelude//cpu/constraints:cpu";

/// Whether `zig` is on `PATH`. Checked once.
pub fn zig_present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        Command::new("zig")
            .arg("version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// `(target triple, source) -> did it compile`, so a header asked about in
/// twenty files is built once per target, not twenty times.
///
/// ponytail: unbounded and never evicted — one process imports a bounded
/// set of projects, so it stays small; add an LRU if that stops being true.
#[derive(Default)]
pub struct ProbeCache(HashMap<(String, String), bool>);

impl ProbeCache {
    fn compiles(&mut self, triple: &str, snippet: &str) -> bool {
        let key = (triple.to_owned(), snippet.to_owned());
        if let Some(hit) = self.0.get(&key) {
            return *hit;
        }
        let ok = syntax_check(triple, snippet);
        self.0.insert(key, ok);
        ok
    }
}

/// Whether `snippet` compiles for `triple` — `zig cc -c` (compile to an
/// object, no link), so this is "does it compile", never "does it link".
///
/// zig 0.15.2's own `-fsyntax-only` is broken (reports `FileNotFound` for
/// any input), hence a real `.c` file and a discarded `.o`. Each call uses
/// a unique stem so parallel project imports do not collide.
fn syntax_check(triple: &str, snippet: &str) -> bool {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let stem = format!(
        "decay-probe-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir();
    let src = dir.join(format!("{stem}.c"));
    let obj = dir.join(format!("{stem}.o"));
    if fs::write(&src, snippet).is_err() {
        return false;
    }

    let status = Command::new("zig")
        .args(["cc", "-target", triple, "-w", "-c"])
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);

    // A `-target` zig has no headers for exits non-zero too: that
    // combination is simply not one decay supports, so "did not compile" is
    // the right answer either way.
    status.map(|s| s.success()).unwrap_or(false)
}

/// The `(abi buck2 value, zig `-target` triple)` pairs decay probes for a
/// CPU on `linux` — glibc and musl. Mirrors `decay_libc_db`'s own arch/abi
/// spelling (arm32 is hard-float EABI either way).
fn linux_targets(cpu: Cpu) -> [(&'static str, String); 2] {
    [
        (
            "gnu",
            format!("{}-linux-{}", cpu.zig_arch(), cpu.glibc_abi()),
        ),
        ("musl", cpu.musl_target()),
    ]
}

/// Build `probe`'s snippet for every `linux` target in the matrix; return
/// the `[abi, cpu]` rows (buck2 constraint values) it compiled for.
pub fn linux_rows(cache: &mut ProbeCache, probe: &CompileProbe) -> Vec<Vec<String>> {
    let snippet = probe.snippet();
    let mut rows = Vec::new();
    for cpu in Cpu::ALL {
        for (abi, triple) in linux_targets(cpu) {
            if cache.compiles(&triple, &snippet) {
                rows.push(vec![abi.to_owned(), cpu.buck2_value().to_owned()]);
            }
        }
    }
    rows
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
    use super::*;

    #[test]
    fn header_rows_match_reality() {
        if !zig_present() {
            eprintln!("skipping: no zig on PATH");
            return;
        }
        let mut cache = ProbeCache::default();

        let present = linux_rows(
            &mut cache,
            &CompileProbe::Header {
                header: "stdio.h".to_owned(),
            },
        );
        assert_eq!(
            present.len(),
            Cpu::ALL.len() * 2,
            "stdio.h should compile for every linux target"
        );

        let absent = linux_rows(
            &mut cache,
            &CompileProbe::Header {
                header: "decay_no_such_header_xyz.h".to_owned(),
            },
        );
        assert!(absent.is_empty(), "a bogus header compiles nowhere");
    }

    #[test]
    fn type_probe_respects_its_prefix() {
        if !zig_present() {
            eprintln!("skipping: no zig on PATH");
            return;
        }
        let mut cache = ProbeCache::default();

        // No prefix: `struct iovec` is undeclared, compiles nowhere.
        let bare = linux_rows(
            &mut cache,
            &CompileProbe::Type {
                name: "struct iovec".to_owned(),
                prefix: String::new(),
            },
        );
        assert!(bare.is_empty());

        // With the header that declares it: compiles everywhere.
        let with_prefix = linux_rows(
            &mut cache,
            &CompileProbe::Type {
                name: "struct iovec".to_owned(),
                prefix: "#include <sys/uio.h>".to_owned(),
            },
        );
        assert_eq!(with_prefix.len(), Cpu::ALL.len() * 2);
    }
}
