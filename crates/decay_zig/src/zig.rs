//! Thin wrappers over `zig cc` / `zig env`. Each function spawns `zig`,
//! does one thing, and reports what happened — no caching, no target
//! matrix, no meson knowledge. Callers build the source string and pass
//! the `-target` triple; that is the whole contract.
//!
//! `zig` is a hard requirement of decay. A missing or unrunnable `zig`
//! panics here rather than degrading to an "unknown" answer.

use std::{
    fs,
    path::PathBuf,
    process::{
        Command,
        Stdio, //
    },
    sync::atomic::{
        AtomicU64,
        Ordering, //
    },
};

/// The zig release decay is developed against. A different one on `PATH`
/// still runs; [`crate`]'s musl probing prints a one-time heads-up.
pub const EXPECTED_VERSION: &str = "0.15.2";

/// A unique temp path stem — `<tmp>/<prefix>-<pid>-<seq>` — so parallel
/// project imports never collide on a probe file.
fn tmp_stem(prefix: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

fn spawn(cmd: &mut Command) -> std::process::Output {
    cmd.output().unwrap_or_else(|e| {
        panic!(
            "`zig` is required by decay but could not be run ({e}); install zig \
             (decay is developed against {EXPECTED_VERSION})"
        )
    })
}

/// `zig version`, trimmed.
pub fn version() -> String {
    String::from_utf8_lossy(&spawn(Command::new("zig").arg("version")).stdout)
        .trim()
        .to_owned()
}

/// `zig env`'s `.lib_dir`: where the install keeps its vendored libc
/// headers, musl source, and glibc `abilists`.
pub fn lib_dir() -> PathBuf {
    let out = spawn(Command::new("zig").arg("env"));
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|line| line.trim().strip_prefix(".lib_dir = \""))
        .and_then(|rest| rest.split('"').next())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("`zig env` output has no `.lib_dir`:\n{text}"))
}

/// Whether `code` compiles for `-target <target>` — `zig cc -c` to a
/// discarded object, so strictly "does it compile", never "does it link".
/// `flags` are extra compiler flags spliced onto the command line verbatim
/// (already vetted by the caller as valid for this target).
pub fn compiles(code: &str, target: &str, flags: &[&str]) -> bool {
    let stem = tmp_stem("decay-zig-cc");
    let src = stem.with_extension("c");
    let obj = stem.with_extension("o");
    if fs::write(&src, code).is_err() {
        return false;
    }
    let output = spawn(
        Command::new("zig")
            .args(["cc", "-target", target, "-w", "-c"])
            .args(flags)
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    // A `-target` zig has no headers for also exits non-zero — that
    // combination is simply not one decay supports, so "did not compile"
    // is the right answer either way.
    output.status.success()
}

/// Whether `code` links for `-target <target>` with each `-l<lib>`. `Ok`
/// on a clean link; `Err` carries the linker's stderr verbatim so a caller
/// can scrape `undefined symbol:` lines out of a deliberate failure.
pub fn link(code: &str, target: &str, libs: &[&str]) -> Result<(), String> {
    let stem = tmp_stem("decay-zig-ld");
    let src = stem.with_extension("c");
    let out = stem.with_extension("out");
    if let Err(e) = fs::write(&src, code) {
        return Err(format!("could not write probe source: {e}"));
    }
    let mut cmd = Command::new("zig");
    cmd.args(["cc", "-target", target, "-w"]).arg(&src);
    for lib in libs {
        cmd.arg(format!("-l{lib}"));
    }
    cmd.arg("-o").arg(&out).stdout(Stdio::null());
    let output = spawn(&mut cmd);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&out);
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// `zig cc -Xclang -ast-dump -fsyntax-only` over `code` for `-target
/// <target>`; returns clang's stdout (the AST dump). The command is
/// expected to exit non-zero on a real translation unit — clang still
/// dumps every `FunctionDecl` it parsed either side of the error, which is
/// all a caller reads back — so failure is not reported, only an empty
/// dump if `zig` produced nothing.
pub fn ast_dump(code: &str, target: &str) -> String {
    let src = tmp_stem("decay-zig-ast").with_extension("c");
    if fs::write(&src, code).is_err() {
        return String::new();
    }
    let output = spawn(
        Command::new("zig")
            .args([
                "cc",
                "-target",
                target,
                "-Xclang",
                "-ast-dump",
                "-fsyntax-only",
            ])
            .arg(&src)
            .stderr(Stdio::null()),
    );
    let _ = fs::remove_file(&src);
    String::from_utf8_lossy(&output.stdout).into_owned()
}
