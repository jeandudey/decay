# decay

`decay` imports a [meson](https://mesonbuild.com/) project into a
[buck2](https://buck2.build/) build, so a project can move onto buck2
without hand-porting its `meson.build` files or losing the configuration
choices they expose.

## Rules

- Always materialize git lfs (`git lfs pull`) when working, no skip before doingo
  anything, otherwise build error.

## How it works

Meson is normally run once per configuration. `decay` instead runs it
variationally: a `meson.build` is evaluated once for every configuration at
once. Each value carries the set of configurations it holds under (a
presence condition, tracked through a hash-consed boolean DAG and checked
with Z3). Whatever a project's `decay.toml` does not pin down stays open,
so a build that lets you turn GLX on and off keeps letting you do that
after import, as a buck2 `select()`, not as a fixed choice baked in at
import time.

The pipeline:

1. **Parse** a `meson.build` into an AST (`decay_meson_ast`,
   `decay_meson_parse`). The latter shells out to meson's own Python parser
   via `pyo3` and lowers its output.
2. **Evaluate** it variationally (`decay_meson_eval`) against an `Oracle`
   that answers what the executor can't know on its own: target machine,
   compiler probes, external dependencies, `find_program`. `decay`'s own
   oracle (`src/oracle.rs`) answers from a project's `decay.toml` and from
   what earlier projects in it already provide (`src/packages.rs`): when one
   imported project's `dependency('x')` names another, decay resolves it
   against that project's own `pkg.generate()` or `.pc`-producing
   `configure_file()`, not a hand-written answer repeating what importing
   that project already determined.
3. **Lower** the result into a backend agnostic build graph
   (`decay_build_ir`): targets, sources, and attributes still carrying
   their presence conditions, with meson forgotten.
4. **Emit** buck2 build files from that graph (`decay_buck2`). Every
   configuration left open becomes a buck2 constraint, and every
   conditional attribute becomes a `select()` over those constraints.

## Goals

- A configuration knob the meson project exposed should still be a knob
  after import, not a value frozen at import time. The generated build
  should offer real `select()`s, not a single flattened answer.
- Things a build graph cannot reach outside itself for (tools via
  `find_program`, the host machine, a dependency nothing imported provides)
  are answered from `decay.toml`, explicitly, rather than guessed or
  silently assumed. A dependency that names another imported project is
  answered by that project instead — decay already knows it in full, so
  `decay.toml` should not have to repeat it.
- Nothing unused should be generated. An option or constraint that nothing
  in the emitted build actually selects on should not appear at all.
- The emitted `BUCK` and constraint files should read like something a
  person would write by hand: buildifier clean, shallow `select()`
  nesting, shared values hoisted, not a naive dump of the decision diagram
  behind them.

See `example/` for a libepoxy import, and `example/decay.toml` for how a
project's escape hatches (`[systems]`, `[probes]`, `[programs]`,
`[dependencies]`) are meant to read.

## Known gaps

- **Wrap support covers both `[wrap-file]` and `[wrap-git]`.** A `[[project]]` entry can now be
  `wrap = "name"` (optionally `version = "..."`, wrapdb's own
  `version-revision` spelling) instead of `repo`/`rev`: `src/wrapdb.rs` and
  `src/wrap_cache.rs` resolve it, and the rest of the pipeline treats it
  exactly like a git project from there (`Packages` resolution, `depends`,
  `decay_build_ir::Origin::Archive` → a buck2 `http_archive` with
  `sub_targets`, same as `git_fetch`). `decay.lock` (`src/lock.rs`) pins
  whatever was resolved — a wrap's wrapdb version when `decay.toml` does not
  pin one, and now also a `[[project]]` `rev` that names a branch or tag
  rather than a commit (resolved once via `git ls-remote`, the same relation
  `Cargo.lock` has to `Cargo.toml`).

  wrapdb has no supported API for this: its `v2` query endpoints answer with
  data that doesn't know about `patch_directory` and don't match what a
  current `.wrap` file says. `src/wrapdb.rs` instead treats wrapdb itself —
  <https://github.com/mesonbuild/wrapdb> — as the source of truth, checked
  out through [`crate::git_cache::GitCache`] the same way any other
  project's git history is: `releases.json` at its root lists every
  project's known versions (newest first, used when `decay.toml` pins none),
  and each `name`/`version` pair is tagged `{name}_{version}`, at which
  `subprojects/{name}.wrap` and, when the wrap carries one,
  `subprojects/packagefiles/{patch_directory}` are exactly what that release
  resolved to — matching `mesonbuild.wrap.wrap.PackageDefinition` and
  `Resolver.apply_patch` in meson's own source, which is what `patch_directory`
  and its `copy_tree` overlay semantics here are modeled on. A bare branch or
  tag name doesn't resolve directly against `GitCache`'s local mirror (its
  fetch lands branches under `refs/remotes/origin/*`, and `checkout` only
  reliably takes a full commit hash), so `wrapdb.rs` resolves one through
  `git_cache::resolve_rev` first, the same as `[[project]]`'s own `rev` does
  in `src/lock.rs`.

  `WrapCache::materialize` downloads and extracts the upstream tarball as
  before, then — when the wrap names a `patch_directory` — copies wrapdb's
  own files for it onto the extracted tree before `decay` ever evaluates it,
  the same recursive overwrite meson's `copy_tree` does. Verified against
  the real `pcre2` wrap (10.48-1): `wrapdb.rs`'s discovery, fetch, and
  overlay all produce the same `meson.build` wrapdb ships, letting `decay`
  evaluate a project whose upstream release has no meson build at all. That
  overlay only ever lands in `decay`'s own working copy, not in the emitted
  `Origin::Archive` (still just the plain upstream tarball's URL and hash) —
  fine for a `patch_directory` that only replaces meson build files (as
  every wrap wrapdb currently publishes does), which the emitted build never
  fetches anyway since `decay` forgets meson and replaces it with generated
  `BUCK` rules. A `patch_directory` that overlaid a file some target's
  `srcs`/`headers`/`template` actually references — none does today, but a
  few packagefiles trees carry non-meson helper files (`.def` export lists,
  `config.h.meson` templates) for other projects — would need that file
  fetched into the archive some other way; `sub_targets` addressing against
  the plain tarball would just fail loudly for it rather than silently
  produce a wrong build, but nothing does that fetch yet.

  `decay.lock` also pins the wrapdb commit a `patch_directory` overlay came
  from (`WrapFile::wrapdb_rev`, `LockedWrap::wrapdb_rev`), not just the
  tarball's own hash or the `[wrap-git]` commit — `source`'s hash or resolved
  commit already made the *source* reproducible, but nothing pinned the
  overlay's own content the same way, only the `{name}_{version}` tag's
  name, and that tag is not actually immutable: wrapdb force-moved
  `ff-nvcodec-headers_11.1.5.1-0` from a `[wrap-git]` wrap to an unrelated
  `[wrap-file]` one after the fact. `wrapdb::fetch` resolves that tag to a
  commit hash once and stamps it onto the result; `wrapdb::patch_dir` then
  takes that hash directly rather than ever re-resolving the tag, so a
  second run overlays the exact same files a first one did even if wrapdb's
  tag has since moved.

  A `[wrap-git]` wrap (`url`/`revision`, plus an optional `patch_directory` —
  wrapdb's `ff-nvcodec-headers_11.1.5.1-0` once carried both) resolves and
  fetches exactly the way an ordinary `[[project]]` `repo`/`rev` does:
  `wrapdb::parse_wrap` returns a `WrapSource::Git { url, revision }`,
  `src/lock.rs` resolves `revision` to a full commit hash through
  `git_cache::resolve_rev` and pins it in `decay.lock` (same as `[[project]]`'s
  own `rev`), and `execute()` in `src/main.rs` checks it out through the same
  shared `GitCache` every git project uses — `Origin::Git` in the emitted
  build, not `Origin::Archive`. A `patch_directory` still applies afterward,
  but never onto `GitCache`'s own checkout directory: that's a shared
  worktree every other project pinned to the same commit trusts unmodified,
  so `WrapCache::materialize_git` only copies it (skipping `.git`) into a
  private directory when there's an overlay to apply, and hands back the
  shared checkout untouched otherwise. No current wrapdb release is actually
  `[wrap-git]` to test this against end-to-end — every one has migrated to
  `[wrap-file]` — so `wrap_cache.rs`'s test for it runs entirely offline,
  against a throwaway local git repo, rather than `#[ignore = "network"]`
  like the `[wrap-file]` one.

  Still open:
  - **The legacy `patch_filename` archive overlay is refused.** No current
    wrapdb release uses it (every one migrated to `patch_directory`), only a
    handful of old pinned versions might still need it, and it has the same
    "can't fetch it into the emitted archive" limitation as a `patch_directory`
    that overlays a referenced non-meson file, above — just without the
    "current releases never hit it" mitigation. `wrapdb::parse_wrap` bails
    with a clear message rather than approximate it.
  - **`[provide] dependency_names` is not read.** A wrap resolves against a
    sibling `dependency()` the same way any other project does — by its own
    `meson.build`'s `declare_dependency()`, matched by name against the wrap's
    `name` — which already has the "only when the looked-up name equals the
    project's `short_name`" narrowness noted below under "`declare_dependency()`
    provide heuristic is narrow." A wrap whose `[provide]` names something
    else would need that gap closed first for `dependency_names` to be worth
    reading.

- **Unsatisfiable constraint.** This could be removed, we need to research if
  select_incompatible is a better option, the message could just be
  unsatisfiable or a custom generated one if we have the data to back it up.

- **has_function.** For common C standard library functions on most operating
  systems we could have this built-in in decay (altough with an option
  to disable it in decay.toml or allow overriding the results), e.g.
  has_function('dlvsym') or whatever should just compile to a select for linux
  and gnu abi. This should be a database built-in into decay, and it must never
  be hand-curated — built by parsing an authoritative data source, the way
  `bindgen` reads a header instead of a person transcribing it.

  Plan, scoped to `has_function` on `linux`+`gnu` first:
  1. Zig ships exactly such a source: `lib/libc/glibc/abilists`, a compact
     binary listing, for every glibc version and target triple, which
     symbols each versioned glibc release exports — the data glibc's own
     project publishes and zig uses to synthesize its glibc stub `.so`s for
     cross-linking. Vendor that one file (pinned to one zig release, noted
     in-tree) into a new crate rather than requiring `zig` installed, since
     it is a plain data file. A refresh means re-fetching that file from a
     newer zig release, never editing a function list by hand.
  2. Parse its binary format (documented by `loadMetaData` and the
     function-inclusion loop in zig's own `src/libs/glibc.zig`) into the set
     of symbol names present in the function table (skip the object table —
     data symbols, not functions) for the `x86_64-linux-gnu` column, unioned
     across libc/libm/libpthread/libdl/librt/libutil/libresolv, since
     `has_function` doesn't care which of those a symbol lives in.
  3. Wire it into `src/oracle.rs` as a fallback `Oracle::probe` consults only
     after a project's own `decay.toml` `[probes]` entry misses — an explicit
     answer there still wins, and a project-wide toggle turns the built-in
     table off entirely.
  4. Buck2's `abi` constraint (`gnu`/`msvc`/`musl`/`unspecified`) is shared
     across every OS — `abi[gnu]` also means mingw on Windows, not just
     glibc — so a glibc-derived fact must select on `os == linux AND abi ==
     gnu` together, not `abi` alone. `Probe` needs a variant that ANDs the
     `Pc` `[systems]` already builds with the one `[probes]`'s
     `Probe::Constraint` already builds, rather than reusing `Probe::Constraint`
     unchanged.
  5. A later pass extends the same vendored file to other glibc target
     columns, and separately to musl/Darwin/mingw once an equally
     authoritative, automatically-parseable source for each is found — and,
     after that, to `has_header` and the rest, which is a harder problem
     (header presence also depends on optional dev packages, not just the
     libc), so it stays its own follow-up.

- **Compile/link probes resolved by `zig cc` at import time.** The
  `has_function` database above answers one probe kind from a parsed table;
  the more general version is to *link-test* a probe against `zig cc
  -target <triple>` once per `(os, cpu, abi)` tuple in decay's configured
  matrix (the `[systems]` set intersected with the constraints left open).
  A probe then stops being a `VarKind::Probe` constraint and becomes a
  presence condition over constraints decay already tracks: compiles for
  every tuple → just `true`, no `select()`; compiles for none → the
  dependent branch is dead and nothing is emitted; compiles for some → a
  `select()` over `os`/`cpu`/`abi`, never a synthetic `has_foo_bar`
  constraint. `zig` is one hermetic cross-compiler with bundled
  glibc/musl/mingw/wasi headers and stubs, pinned to one release the same
  way `decay_libc_db/data/glibc-abilists` already is; an explicit
  `decay.toml [probes]` answer still wins first, and a project-wide toggle
  turns the whole mechanism off.

  **Landed (first slice).** `cc.has_header`, `cc.has_type`, and
  `cc.compiles` — bare shapes only (no project `args:`/`dependencies:` the
  importer cannot replay; `has_header` also skips anything but a plain
  header path) — are answered by `src/probe.rs`: `zig cc -c` (compile to a
  discarded object; zig 0.15.2's own `-fsyntax-only` is broken) for each
  `(abi, cpu)` in `decay_libc_db::Cpu::ALL` × {gnu, musl}, on `linux` only.
  The result is `oracle::Probe::Matrix` — true on the rows that compiled,
  settled *false* (no knob) on every other `(abi, cpu)` under `linux`, and
  left open only off `linux` where nothing was built. `decay_meson_eval`
  gains `oracle::CompileProbe` (the reconstructed translation unit) and
  `Oracle::compile_probe`; `src/config.rs` gains `probe_with_zig` (default
  true, no effect without `zig` on `PATH`); an explicit `[probes]` entry
  still wins first via the existing `Oracle::probe` path.

  Still in scope, not yet done:
  - **`cc.links`** — needs a real link (output file, `main` handling), not
    just `-c`; `CompileProbe` has no `Links` variant yet.
  - **compile-time `cc.sizeof` of a *type*** — the `static_assert` binary
    search meson falls back to when it cannot run. Goes through the
    `SizeAnswer` path (`Oracle::type_size`), not `compile_probe`.
  - **Other systems / abis** — only `linux` + {gnu, musl} today, mirroring
    `builtin_has_function`. Windows/mingw, macOS, and `x86`/`arm` beyond
    `Cpu::ALL` are a later pass; `msvc` is unreachable (`zig` has no MSVC
    headers).

  Deferred — out of scope, each its own follow-up:
  - **`cc.has_argument()` / `has_link_argument()` / `has_multi_arguments()`
    are compiler-specific.** `zig cc` *is* clang, so "does `-Wfoo` exist"
    can diverge from a gcc toolchain. These stay open constraints (default
    `true`, as today) or a `decay.toml [probes]` answer until decay
    standardizes the cxx toolchain of its emitted builds on clang/zig — the
    same direction as the hermetic-`python3`-in-`toolchains//` note below.
  - **`cc.run()` proper, `cc.alignment()`'s value, `cc.compute_int()`.**
    These need the probe *executed*, not just linked, and decay does not
    cross-run (no qemu). They stay on the `decay.toml` path — see
    "`cc.compute_int()` has no configured answer" below, which this does not
    change.
  - **Kernel/libc-header-vintage probes.** `HAVE_FUTEX_TIME64` and kin
    resolve against `zig`'s *bundled* Linux headers — one pinned answer.
    Better than today's "default to `true` then fail to compile" (see "An
    unanswered probe defaults to `true`" below), but it is a zig-release
    pin, not a real per-host fact; real Linux systems still disagree.
  - **Probe context threading.** Meson runs a probe with the project's
    `c_args` and each named dependency's cflags/include paths. `zig cc`
    needs the same flags for `cc.has_header('x.h', dependencies: dep)` and
    friends to answer correctly; without them a header behind a dependency's
    include dir reads as absent.
  - **Determinism / golden tree.** Probe answers get baked into the emitted
    build, so `example/`'s golden tree shifts whenever the pinned `zig`'s
    libc or headers change — the same maintenance tradeoff the abilists
    database already accepted.

- **Python3 genrules.** the genrules using python should try to use python rules if possible
  to define the scripts, and only fallback to genrule with an override in decay.toml if it
  turns out it doesn't work with buck2 rules, also python3 should point to an hermetic
  python3 executable as defined in toolchains//.

- **Computed dict keys.** Meson allows any expression as a dict key
  (`{ 'cxx-@0@'.format(std): {...} }`, glib's test suite). `Expr::Dict`
  currently holds `Vec<(String, Expr)>` with a parallel `order: Vec<String>`;
  the parser's `expect_key()` (`decay_meson_parse/src/node.rs`) only accepts
  a string- or id-shaped key and bails otherwise. The fix is to carry keys
  as `Expr` (`Vec<(Expr, Expr)>`), thread that through `lower.rs`'s
  `Node::Dict` arm and `Interp`'s `Expr::Dict` evaluation, and evaluate each
  key like any other expression. Needed for glib with `tests` enabled and
  for any project that builds dict keys with `.format()` / concatenation.
  `example/decay.toml` pins `options.tests = false` for glib to avoid it
  for now.

- **`declare_dependency(sources: [...])` with compilable sources.** decay
  routes every `sources:` entry into the interface target's `headers`
  (`fn_declare_dependency` in `decay_meson_eval/src/builtins.rs`), and
  `decay_buck2` emits a `Kind::Interface` target with no `srcs` — so a
  "copylib" like gvdb (`declare_dependency(sources: ['gvdb-builder.c',
  'gvdb-reader.c'], ...)`) comes out as an empty `cxx_library` with the
  `.c` files listed under `exported_headers`, and a consumer never compiles
  them. The fix is to split `sources:` into real headers vs. compiled
  sources and emit the latter as `srcs` on a compiled library (or fold
  them into each consumer). Visible today in
  `example/third-party/meson/gvdb/BUCK`.

- **Configuration-dependent install paths and `.pc` variables.**
  `Attrs.install_dir` is `Option<String>` and `src/packages.rs`'s `Package.
  variables` is a flat `Vec`. So a `configure_file()` whose `output:` or
  `install_dir:` varies by configuration (glib's systemtap `.stp`, keyed on
  `cpu_family`) aborts with "expected a single string … N variants", and
  `single_valued_pairs` silently drops any `pkg.generate()` /
  `declare_dependency()` variable that came out configuration-dependent
  (glib's `multiarch`-keyed `giomoduledir`) rather than emitting a
  `select()`. Both want the value carried variationally through to emit.

- **`link_args:`/`c_args:` embedding a path to a same-project file are now
  resolved instead of opaque strings.** Surfaced by pcre2's version scripts
  (`'-Wl,--version-script,@0@/lib@1@.sym'.format(meson.current_build_dir(),
  lib)`, `meson.current_build_dir()` having no real directory to answer with
  and fabricating `"."`) and, the same shape one level simpler, zlib's and
  libglvnd's own version scripts naming a plain file already in their
  checkout via `current_source_dir()`/a bare relative path. `Attrs.
  compile_args`/`link_args` (`decay_build_ir/src/lib.rs`) are now
  `Variational<Flag>` — `Flag::Literal` unchanged, `Flag::File(prefix, Source)`
  for a flag whose trailing path names something the project's own graph
  provides. `Interp::capture_flag` (`decay_meson_eval/src/lib.rs`) recognizes
  it at the point `link_args:`/`compile_args:` is captured: the flag's last
  comma- or space-delimited word, normalized (`normalize_path`, which is what
  turns `current_source_dir()`'s fabricated leading `/` or `current_build_dir()`'s
  `./` into a clean relative path), checked first against every
  `configure_file()`/`custom_target()` output this project's graph already
  declares, then against the project's own checkout (`Sources::exists`) —
  anything else comes back unchanged. `decay_buck2`'s `flag()` renders a match
  as `$(location ...)` spliced into the flag's literal prefix — buck2 expands
  the macro and adds the referenced target as an implicit dependency the same
  way it does anywhere else a string attribute takes one — and
  `referenced_files()` picks up a `Flag::File` the same way it already does
  `Source::File`/`CmdArg::File`, so a plain checked-out file like `zlib.map`
  reaches the http_archive's own `sub_targets`.

  Still narrow on purpose: only the flag's *own* trailing word is checked
  (not an arbitrary embedded substring), and an ambiguous match — two targets
  declaring the same output name — is not disambiguated, just left as a
  literal. Nothing has hit that yet.

- **Conditional `continue` in a `foreach` over a static list.** `break` now
  splits the remaining iterations under its negation (`Flow::Break(Pc)` in
  `decay_meson_eval/src/lib.rs`); `continue` still bails ("has no static
  translation") when it is partial. It needs the same treatment — the
  statements after a `continue` should run under the complement of the
  condition it fired under.

- **`windows.compile_resources` / `fs.copyfile` are minimal.**
  `compile_resources` drops `args:` (resource-compiler flags) and
  `include_directories:` (RC search paths); `fs.copyfile` emits a `cp`
  command, which a Windows genrule does not have. Both matter for the
  Windows target that is a priority.

  A `.rc` source itself is no longer left in a `cxx_library`, where buck2
  never runs the resource compiler over it. `split_resources`
  (`decay_meson_eval/src/builtins.rs`) peels every `.rc` — whether from
  `library()`/`executable()` sources, `declare_dependency(sources:)`, or a
  generated `configure_file()` output — out into its own
  `Kind::WindowsResource` target (`decay_build_ir`), which `decay_buck2`
  emits as a `windows_resource` rule wired back into the consumer through
  `link_with` → `deps`/`exported_deps`; the `os[windows]` gating falls out of
  the pulled entries' presence conditions. Still `srcs`-only: a `.rc` that
  `#include`s a project header needs `include_directories`/`headers` emitted
  on that rule too (the `include_directories:` gap above).

- **The `python` module is a stub.** Only `import('python').
  find_installation()` (resolved like any `[programs]` entry) and
  `.language_version()` (fabricated `"3.12"`, the way `cc.version()` is).
  A project that builds Python extension modules needs `.extension_module()`,
  `.dependency()`, `.install_sources()`, `.get_install_dir()`,
  `.get_variable()`, none of which exist.

- **`cc.compute_int()` has no configured answer.** It falls back to the
  call's `guess:` and errors without one. Like `[sizeof]` / `[alignment]`,
  the no-guess case should be answerable from `decay.toml`.

- **`run_command()` is refused outright.** Some projects call it for
  harmless reads (a `VERSION` file). A read-only subset, or a `decay.toml`
  answer, would unblock them without baking in a machine-specific result.

- **No end-to-end import test.** The only tests are the `schedule` and
  `config` unit tests. An evaluator regression that breaks the `example/`
  import (glib, libepoxy, …) would not be caught. Add a test that runs
  `decay` on `example/` and diffs the generated tree against a
  committed golden copy — this also pins `-j1` == `-jN`.

- **`declare_dependency()` provide heuristic is narrow.** A sibling
  `dependency('x')` resolves only against a `declare_dependency()` in
  project `x`'s *root* `meson.build`, last-call-wins, and only when the
  looked-up name equals the project's `short_name`. A wrap whose
  `[provide] dependency_names` differs from the directory name, or a project
  with several root `declare_dependency()` calls, would not resolve.

- **An unanswered probe defaults to `true`.** `probe_var()`
  (`decay_meson_eval/src/lib.rs`) gives every `VarKind::Probe` constraint a
  hardcoded `default = 0` ("true"), reasoning that "a compiler capability ...
  is what a working toolchain normally reports." That is right for most
  `cc.has_argument()`/`cc.compiles()` checks, but wrong for one that is
  really a *kernel/libc vintage* question with no constraint decay tracks:
  glib's `HAVE_FUTEX_TIME64` (`cc.compiles(..., name: 'futex_time64(2)
  system call')`) defaults to present and fails to compile
  (`gthreadprivate.h`'s `__NR_futex_time64` branch) on any host whose
  `<sys/syscall.h>` predates it — this one can't be tied to `[systems]` or
  any other existing constraint the way `has_header:crt_externs.h` (now
  answered via the `darwin` system) can, because real Linux systems
  genuinely disagree on it. Fixing it means either running the real
  compiler against the probe at import time (a bigger change to the
  "importer never shells out to `cc`" design) or letting `decay.toml`
  override just the default half of a `[probes]` entry, independent of
  fixing/tying it. Until then, building a project with such a probe needs an
  explicit `-c` override for the affected constraint.

- **Better diagnostics.** If something fails to import because it needs user input
  we should provide a way for the user to fix it if possible. If it is something
  we don't have implemented then we should provide that. Ideally we should collect
  unimplemented functions and methods in meson and either provide these in the
  program to let the user know it hasn't been implemented, and also to keep a
  list here in known gaps.

- **has_function compaction.** This still uses `has_function_memalign[true]` when it is not necessary,
  either the function exists for glibc for the combinations _we_ check or not. Ideally
  has_function_memalign should be gone completely from the generated constraints and
  build file. That is the main reason for the libc database.

```
        "prelude//os/constraints:os[linux]": select({
            "prelude//abi/constraints:abi[gnu]": select({
                "//third-party/meson/constraints:has_function_memalign[true]": " '#define HAVE_MEMALIGN 1'",
                "//third-party/meson/constraints:has_function_memalign[false]": select({
                }),
```
  Ideally this should be

```
        "prelude//os/constraints:os[linux]": select({
            "prelude//abi/constraints:abi[gnu]": select({
                "prelude//cpu/constraints:cpu[arm32]": " '#define HAVE_MEMALIGN 1'",
                "prelude//cpu/constraints:cpu[arm64]": " '#define HAVE_MEMALIGN 1'",
                "prelude//cpu/constraints:cpu[riscv64]": " '#define HAVE_MEMALIGN 1'",
                "prelude//cpu/constraints:cpu[x86_32]": " '#define HAVE_MEMALIGN 1'",
                "prelude//cpu/constraints:cpu[x86_64]": " '#define HAVE_MEMALIGN 1'",
                "DEFAULT": "",
```

- **Adding meson specific buck2 rules.** We shall be able to take the .h.in files with `#mesondefine` et all
  and just substitute correctly instead of assuming the layout of the file with `.set` calls, output should
  be more or less identical to what meson generates.

- **add_test_setup.** Implement this, I'm tired of seeing this in the output and it should more or less
  work with buck2 too.

- **Libraries provided by the compiler should exist or not.** See this:

``` 
# external dependency `lib:m`: `m` is available
constraint(
    name = "m",
    values = ["true", "false"],
    default = "false",
    visibility = ["PUBLIC"],
)
```

  It should just be true for the systems that provide it, that's it, libc DB needs this.

  Partly done: `find_library()` for a libc-split library MSVC has no
  standalone `.lib` for (`m`, `dl`, `rt`, `pthread`, `resolv`, `nsl`,
  `socket`, `anl`, `crypt`, `util`, `execinfo` — `is_crt_provided_lib` in
  `decay_buck2`) now emits its `-l…` under `select({ abi[msvc]: [], DEFAULT:
  … })`, so it contributes nothing on the MSVC ABI (matching meson's own
  not-found there). The remaining half — settling the `m[true/false]` knob to
  *true* on the systems that do provide it, instead of an open probe — still
  wants the libc DB.

- **Support all of meson wrapdb.** This should be the biggest showcase and smoke test for decay, we should be able to import all of the wrapdb projects.

- **dependency('threads') is a builtin.** `fn_dependency` special-cases it
  (`External::Threads`): always found, never a `threads[true/false]` knob, and
  `decay_buck2` renders it as a real `cxx_library` carrying `-pthread`
  everywhere except `abi[msvc]` (MSVC / clang-cl put threads in the CRT and
  reject the flag). Still overridable with `dependencies.threads = "//x"` in
  `decay.toml`. It did not end up needing the libc DB — `-pthread` is a
  compiler-driver convention, and `abi[msvc]` is the whole of the exception.

- **Pretty print errors.** Use annotate-snippets crate from rust-lang for this
  current errors are crap.

- **This should not be in each project.**

```
cxx_library(
    name = "threads",
    exported_preprocessor_flags = select({
        "prelude//abi/constraints:abi[msvc]": [],
        "DEFAULT": ["-pthread"],
    }),
    exported_linker_flags = select({
        "prelude//abi/constraints:abi[msvc]": [],
        "DEFAULT": ["-pthread"],
    }),
    visibility = ["PUBLIC"],
)
```

  The libraries generated by Meson that are common to all projects (only common
  in the sense of builtin ones like threads).

- **This should not happen.** zlib.git name for an archive.

```
 http_archive(
    name = "zlib.git",
    urls = ["https://zlib.net/zlib-1.3.2.tar.xz"],
    sha256 = "d7a0654783a4da529d1bb793b7ad9c3318020af77667bcae35f95d0e42a792f3",
```
