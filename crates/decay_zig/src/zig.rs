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

/// Write a probe's source, panicking on failure: a probe that never ran is
/// not a probe that failed.
fn write_src(src: &std::path::Path, code: &str) {
    if let Err(e) = fs::write(src, code) {
        panic!("could not write zig probe source `{}`: {e}", src.display());
    }
}

fn spawn(cmd: &mut Command) -> std::process::Output {
    cmd.output().unwrap_or_else(|e| {
        panic!(
            "`zig` is required by decay but could not be run ({e}); install zig \
             (decay is developed against {EXPECTED_VERSION})"
        )
    })
}

/// Panic unless a failed `zig cc` run failed because of what it was asked to
/// build. A probe reads a non-zero exit as "absent", so zig itself failing
/// (an unwritable cache, a crash, a signal) must not be mistaken for one:
/// that would quietly turn every header and library off.
///
/// A real verdict names the probe's own source (a diagnostic anchored in
/// it), or is one of the few driver/linker refusals that are about the
/// request itself: a flag clang does not take, a missing library, an
/// unresolved symbol, a target zig has no libc for.
fn check_verdict(output: &std::process::Output, src: &std::path::Path) {
    const VERDICTS: &[&str] = &[
        "Unknown Clang option",
        "unsupported option",
        "unknown argument",
        // An ISA flag the target lacks (`-mssse3` on aarch64).
        "has no LLVM CPU feature named",
        "unsupported argument",
        "unknown target CPU",
        "is not supported for target",
        "unable to find dynamic system library",
        "unable to find library",
        "undefined symbol",
        "unable to provide libc",
    ];
    let stderr = String::from_utf8_lossy(&output.stderr);
    let names_src = src
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| stderr.contains(n));
    if output.status.code().is_some() && (names_src || VERDICTS.iter().any(|v| stderr.contains(v)))
    {
        return;
    }
    panic!(
        "`zig cc` failed for a reason unrelated to the probe ({}); refusing to read it as \
         \"absent\":\n{stderr}",
        output.status
    );
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
    write_src(&src, code);
    let output = spawn(
        Command::new("zig")
            .args(["cc", "-target", target, "-w", "-c"])
            .args(flags)
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .stdout(Stdio::null()),
    );
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    // A `-target` zig has no headers for also exits non-zero — that
    // combination is simply not one decay supports, so "did not compile"
    // is the right answer either way.
    if !output.status.success() {
        check_verdict(&output, &src);
    }
    output.status.success()
}

/// Whether `code` links for `-target <target>` with each `-l<lib>`. `Ok`
/// on a clean link; `Err` carries the linker's stderr verbatim so a caller
/// can scrape `undefined symbol:` lines out of a deliberate failure.
pub fn link(code: &str, target: &str, libs: &[&str]) -> Result<(), String> {
    let stem = tmp_stem("decay-zig-ld");
    let src = stem.with_extension("c");
    let out = stem.with_extension("out");
    write_src(&src, code);
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
        check_verdict(&output, &src);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_header_is_a_verdict() {
        assert!(!compiles(
            "#include <decay_no_such_header.h>\n",
            "x86_64-linux-gnu",
            &[]
        ));
    }

    #[test]
    fn an_isa_flag_the_target_lacks_is_a_verdict() {
        assert!(!compiles("int x;\n", "aarch64-linux-gnu", &["-mssse3"]));
    }

    #[test]
    #[should_panic(expected = "unrelated to the probe")]
    fn zig_failing_on_its_own_is_not_absent() {
        compiles("int x;\n", "bogus-triple", &[]);
    }
}
