//! Generates the musl half of `decay_libc_db`'s `has_function` database, for
//! every architecture in [`cpu::Cpu::ALL`].
//!
//! musl publishes no zig-style abilist of what it exports the way glibc does
//! — it has no symbol versioning, so zig just compiles it from source per
//! target instead of shipping a stub-generation database for it. So instead
//! this asks `zig cc` itself, the way meson asks a real compiler, and in two
//! steps that keep the answer entirely musl's own:
//!
//! 1. **Candidates.** Every function musl's own headers (vendored inside
//!    zig, under `<lib_dir>/libc/include/generic-musl` — arch-independent;
//!    each arch's own `<arch>-linux-musl` only adds `bits/*.h`, which this
//!    skips like every other implementation-detail `bits` directory) declare,
//!    found by handing zig's own `clang` an umbrella translation unit that
//!    `#include`s every one of them (with `_ALL_SOURCE` defined, or its GNU-
//!    and BSD-extension declarations stay hidden behind musl's own
//!    feature-test guards) and reading the `FunctionDecl` nodes back out of
//!    `-Xclang -ast-dump`. Real parsing by the real C frontend, not a
//!    hand-typed function list, and not glibc's — musl's own declared API.
//!    Run once per arch (rather than once, reused): the header text is the
//!    same, but what clang's preprocessor keeps from it depends on that
//!    arch's predefined macros.
//! 2. **Verification.** Declaring every candidate `extern` in one C file,
//!    taking each one's address, and linking it for that arch. Musl exports
//!    whichever candidates the linker did *not* report undefined. A name
//!    only *declared*, never defined for this target (`alloca`, a `static
//!    inline` like `__bswap_32`, an unimplemented
//!    `pthread_mutexattr_getprioceiling`) fails here even though step 1
//!    found it, which is the correct answer.
//!
//! This runs once at decay's own build time (this crate's build script), not
//! at import time — the emitted database is what `src/lib.rs` embeds and
//! answers from, so importing a project still never shells out to a
//! compiler. It does mean building `decay_libc_db` itself requires `zig` on
//! `PATH`, the same way it already requires a Python (for `pyo3`, elsewhere
//! in this workspace) — see `data/PROVENANCE.md` for the pinned release this
//! was developed against.
#[path = "src/cpu.rs"]
mod cpu;

use {
    cpu::Cpu,
    std::{
        collections::BTreeSet,
        env, fs,
        path::{Path, PathBuf},
        process::Command, //
    },
};

/// The zig release `data/PROVENANCE.md` pins glibc's abilist to. musl has no
/// symbol versioning to speak of, so a different installed release is not
/// expected to change the answer — this is a heads-up, not a hard
/// requirement.
const EXPECTED_ZIG_VERSION: &str = "0.15.2";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/cpu.rs");

    check_zig_version();

    let lib_dir = zig_lib_dir();
    let header_root = lib_dir.join("libc").join("include").join("generic-musl");
    let headers = find_headers(&header_root);
    assert!(
        !headers.is_empty(),
        "found no musl headers under {}; is `zig env`'s lib_dir right?",
        header_root.display(),
    );

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let mut code = String::from(
        "pub(crate) fn musl_symbols(cpu: Cpu) -> &'static [&'static str] {\n    match cpu {\n",
    );
    for cpu in Cpu::ALL {
        let candidates = declared_functions(cpu, &headers);
        let present = link_probe(cpu, &candidates);
        code.push_str(&format!("        Cpu::{cpu:?} => &[\n"));
        for name in &candidates {
            if present.contains(name) {
                code.push_str(&format!("            {name:?},\n"));
            }
        }
        code.push_str("        ],\n");
    }
    code.push_str("    }\n}\n");
    fs::write(Path::new(&out_dir).join("musl_symbols.rs"), code)
        .expect("writing generated musl_symbols.rs");
}

fn zig(args: &[&str]) -> std::process::Output {
    Command::new("zig").args(args).output().expect(
        "`zig` not found on PATH — decay_libc_db needs zig (pinned release: see \
         data/PROVENANCE.md) installed at build time to derive its musl database, the \
         same way pyo3 needs a Python installed to build against",
    )
}

fn check_zig_version() {
    let output = zig(&["version"]);
    let version = String::from_utf8_lossy(&output.stdout);
    let version = version.trim();
    if version != EXPECTED_ZIG_VERSION {
        println!(
            "cargo:warning=decay_libc_db's musl database was developed against zig \
             {EXPECTED_ZIG_VERSION}; found {version} on PATH. musl has no symbol \
             versioning, so this is unlikely to matter, but a resulting difference in \
             has_function(Libc::Musl, ..) would come from here."
        );
    }
}

/// `zig env`'s `lib_dir`: where the zig install keeps its vendored musl
/// headers and source, alongside glibc's `abilists` (`data/PROVENANCE.md`).
fn zig_lib_dir() -> PathBuf {
    let output = zig(&["env"]);
    let text = String::from_utf8(output.stdout).expect("`zig env` output was not UTF-8");
    let path = text
        .lines()
        .find_map(|line| line.trim().strip_prefix(".lib_dir = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("`zig env` output missing `.lib_dir`");
    PathBuf::from(path)
}

/// Every `.h` file under `root`, recursing into subdirectories except any
/// named `bits` — musl's own convention (shared with glibc) for "implementation
/// detail, not meant to be `#include`d on its own", already pulled in by
/// whichever public header needs it.
fn find_headers(root: &Path) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
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
/// scanner.
fn declared_functions(cpu: Cpu, headers: &[PathBuf]) -> BTreeSet<String> {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let umbrella_path = Path::new(&out_dir).join(format!("musl_umbrella_{}.c", cpu.zig_arch()));

    let mut src = String::from(
        // Without this, musl's own `features.h` hides every GNU/BSD
        // extension declaration behind a feature-test guard, and this
        // would only ever see plain POSIX names.
        "#define _ALL_SOURCE 1\n",
    );
    for header in headers {
        src.push_str(&format!("#include \"{}\"\n", header.display()));
    }
    fs::write(&umbrella_path, src).expect("writing musl_umbrella.c");

    // Not expected to succeed: musl's own header set has at least one
    // internal redefinition when every header lands in one translation
    // unit this way (`max_align_t`, `bits/alltypes.h` vs. a compat shim).
    // `-fsyntax-only` still dumps every `FunctionDecl` it parsed either
    // side of that, which is all this reads back.
    let target = cpu.musl_target();
    let output = zig(&[
        "cc",
        "-target",
        &target,
        "-Xclang",
        "-ast-dump",
        "-fsyntax-only",
        umbrella_path.to_str().expect("OUT_DIR path is not UTF-8"),
    ]);
    let ast = String::from_utf8_lossy(&output.stdout);

    ast.lines()
        .filter(|line| line.contains("FunctionDecl"))
        .filter_map(|line| {
            // A `FunctionDecl` node's name is always the identifier right
            // before its quoted type, e.g. `...col:7 implicit memcpy 'void
            // *(void *, const void *, unsigned long)' extern`.
            let name = line.split('\'').next()?.trim_end().rsplit(char::is_whitespace).next()?;
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

/// Which of `candidates` a real `zig cc` link for `cpu` resolves, by
/// declaring all of them `extern` in one C file, taking each one's address
/// (forcing a real reference, not a compiler builtin substitution), and
/// reading the linker's "undefined symbol" diagnostics back out — one link
/// for every candidate at once, rather than one per name.
fn link_probe(cpu: Cpu, candidates: &BTreeSet<String>) -> BTreeSet<String> {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let src_path = Path::new(&out_dir).join(format!("probe_musl_{}.c", cpu.zig_arch()));
    let exe_path = Path::new(&out_dir).join(format!("probe_musl_out_{}", cpu.zig_arch()));

    let mut src = String::new();
    for name in candidates {
        src.push_str(&format!("extern int {name}();\n"));
    }
    src.push_str("void *_decay_libc_db_refs[] = {\n");
    for name in candidates {
        src.push_str(&format!("    (void*)&{name},\n"));
    }
    src.push_str("};\nint main(void) { return 0; }\n");
    fs::write(&src_path, src).expect("writing probe_musl.c");

    let target = cpu.musl_target();
    let output = Command::new("zig")
        .args(["cc", "-target", &target, "-w"])
        .arg(&src_path)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .expect("zig invocation failed (see check_zig_version's message above)");

    // Expected to fail: most candidates are declared for some other target
    // or configuration and will be reported undefined here. A real failure
    // unrelated to an undefined symbol (a `zig` too old for this `-target`,
    // say) would also exit non-zero but report no "undefined symbol" lines
    // at all — which would otherwise read as "musl exports every
    // candidate", silently and wrongly. Catch that rather than trust an
    // empty diff.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let missing: BTreeSet<String> = stderr
        .lines()
        .filter_map(|line| line.trim().strip_prefix("ld.lld: error: undefined symbol: "))
        .map(str::to_owned)
        .collect();
    assert!(
        !missing.is_empty(),
        "zig cc -target {target} reported no undefined symbols at all out of {} \
         candidates — probe_musl.c likely failed to build for an unrelated reason \
         instead of linking successfully; zig's actual output was:\n{stderr}",
        candidates.len(),
    );

    candidates.difference(&missing).cloned().collect()
}
