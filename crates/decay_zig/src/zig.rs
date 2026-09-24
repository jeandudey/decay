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

/// Run `zig`, once more if it dies by a signal. zig 0.15 can crash when
/// parallel runs build the same target's libc into a cold cache; a second
/// run finds it built. A second crash is left for [`check_verdict`].
fn spawn(cmd: &mut Command) -> std::process::Output {
    let run = |cmd: &mut Command| {
        cmd.output().unwrap_or_else(|e| {
            panic!(
                "`zig` is required by decay but could not be run ({e}); install zig \
                 (decay is developed against {EXPECTED_VERSION})"
            )
        })
    };
    let output = run(cmd);
    if output.status.code().is_some() {
        return output;
    }
    run(cmd)
}

/// Panic when a failed `zig cc` run failed because of zig itself rather than
/// the probe. A probe reads a non-zero exit as "absent", so an unwritable
/// cache, a crash or a signal must not be mistaken for one: that would
/// quietly turn every header and library off.
///
/// Clang's refusals take too many shapes to list (a diagnostic in the source,
/// in `<inline asm>`, a driver or linker complaint), so this recognises zig's
/// own failures instead.
fn check_verdict(output: &std::process::Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_zig_failure(output.status.code(), &stderr) {
        panic!(
            "`zig cc` failed for a reason unrelated to the probe ({}); refusing to read it \
             as \"absent\":\n{stderr}",
            output.status
        );
    }
}

/// Whether a failed run's exit code and stderr point at zig itself: killed
/// by a signal, nothing said at all, zig panicking, a target triple it cannot
/// parse, or one of zig's own error names (`...: NotDir`, `...: OutOfMemory`)
/// closing an `error:` line.
fn is_zig_failure(code: Option<i32>, stderr: &str) -> bool {
    if code.is_none() || stderr.trim().is_empty() {
        return true;
    }
    stderr.lines().any(|line| {
        let line = line.trim();
        line.starts_with("thread ") && line.contains("panic")
            || line.starts_with("panic:")
            || line.starts_with("error: unknown architecture")
            || line.starts_with("error: unknown operating system")
            || line.starts_with("error:")
                && line
                    .rsplit_once(": ")
                    .is_some_and(|(_, last)| is_zig_error_name(last))
    })
}

/// `NotDir`, `AccessDenied`, `OutOfMemory`: an identifier in zig's error-set
/// spelling, which clang never uses for a diagnostic.
fn is_zig_error_name(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && word.len() > 2
        && word.chars().all(|c| c.is_ascii_alphanumeric())
        && word.chars().skip(1).any(|c| c.is_ascii_lowercase())
        && word.chars().skip(1).any(|c| c.is_ascii_uppercase())
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
        check_verdict(&output);
    }
    output.status.success()
}

/// The byte size of `symbol` once `code` compiles for `-target <target>`,
/// read back from the assembly's `.size <symbol>, N` directive — how a probe
/// gets a compile-time number out without running anything: it declares
/// `const char symbol[EXPR]`. `None` when `code` does not compile.
///
/// ponytail: ELF only (`.size` is an ELF directive), which is every target
/// decay compile-probes.
pub fn symbol_size(code: &str, target: &str, flags: &[&str], symbol: &str) -> Option<u64> {
    let stem = tmp_stem("decay-zig-size");
    let src = stem.with_extension("c");
    let asm = stem.with_extension("s");
    write_src(&src, code);
    let output = spawn(
        Command::new("zig")
            .args(["cc", "-target", target, "-w", "-S"])
            .args(flags)
            .arg(&src)
            .arg("-o")
            .arg(&asm)
            .stdout(Stdio::null()),
    );
    let text = fs::read_to_string(&asm).unwrap_or_default();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&asm);
    if !output.status.success() {
        check_verdict(&output);
        return None;
    }
    let size = text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(".size")?.trim_start();
        let (name, n) = rest.split_once(',')?;
        (name.trim() == symbol).then(|| n.trim().parse().ok())?
    });
    Some(size.unwrap_or_else(|| {
        panic!("`zig cc -S` for {target} compiled but emitted no `.size {symbol}`:\n{text}")
    }))
}

/// Whether `code` links for `-target <target>` with each `-l<lib>`. `Ok`
/// on a clean link; `Err` carries the linker's stderr verbatim so a caller
/// can scrape `undefined symbol:` lines out of a deliberate failure.
pub fn link(code: &str, target: &str, libs: &[&str]) -> Result<(), String> {
    let flags: Vec<String> = libs.iter().map(|lib| format!("-l{lib}")).collect();
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    link_with(code, target, &flags)
}

/// Whether `code` compiles and links into an executable for `-target
/// <target>`, with `flags` (compiler flags and `-l…`) spliced on verbatim.
pub fn links(code: &str, target: &str, flags: &[&str]) -> bool {
    link_with(code, target, flags).is_ok()
}

fn link_with(code: &str, target: &str, flags: &[&str]) -> Result<(), String> {
    let stem = tmp_stem("decay-zig-ld");
    let src = stem.with_extension("c");
    let out = stem.with_extension("out");
    write_src(&src, code);
    let output = spawn(
        Command::new("zig")
            .args(["cc", "-target", target, "-w"])
            .arg(&src)
            .args(flags)
            .arg("-o")
            .arg(&out)
            .stdout(Stdio::null()),
    );
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&out);
    if output.status.success() {
        Ok(())
    } else {
        check_verdict(&output);
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
    fn inline_asm_rejection_is_a_verdict() {
        let code = "int main(void) { __asm__(\".func meson_test\\n.endfunc\"); return 0; }\n";
        assert!(!compiles(code, "aarch64-linux-gnu", &[]));
    }

    #[test]
    fn classifies_zig_failures() {
        let zig = [
            "error: unable to open global cache directory 'x': NotDir",
            "error: unable to create compilation: OutOfMemory",
            "thread 1234 panic: reached unreachable code",
            "error: unknown architecture: 'bogus'",
            "",
        ];
        for stderr in zig {
            assert!(is_zig_failure(Some(1), stderr), "{stderr:?}");
        }
        assert!(is_zig_failure(
            None,
            "b.c:1:10: fatal error: 'x.h' file not found"
        ));

        let verdicts = [
            "b.c:1:10: fatal error: 'nope.h' file not found",
            "<inline asm>:1:1: error: unknown directive",
            "error: Unknown Clang option: '-fbogus'",
            "error: target architecture aarch64 has no LLVM CPU feature named 'ssse3'",
            "zig: error: unsupported option '-mfpu=' for target 'x86_64-linux-gnu'",
            "error: unable to find dynamic system library 'nope' using strategy 'paths_first'",
            "ld.lld: error: undefined symbol: f",
        ];
        for stderr in verdicts {
            assert!(!is_zig_failure(Some(1), stderr), "{stderr:?}");
        }
    }

    #[test]
    fn symbol_size_reads_a_compile_time_number() {
        let code = "const char v[sizeof(long double)] = {0};\n";
        assert_eq!(symbol_size(code, "x86_64-linux-gnu", &[], "v"), Some(16));
        assert_eq!(symbol_size(code, "x86-linux-musl", &[], "v"), Some(12));
        assert_eq!(symbol_size(code, "arm-linux-musleabihf", &[], "v"), Some(8));
        let missing = "const char v[sizeof(struct decay_nope)] = {0};\n";
        assert_eq!(symbol_size(missing, "x86_64-linux-gnu", &[], "v"), None);
    }

    #[test]
    #[should_panic(expected = "unrelated to the probe")]
    fn zig_failing_on_its_own_is_not_absent() {
        compiles("int x;\n", "bogus-triple", &[]);
    }
}
