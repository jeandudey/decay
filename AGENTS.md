# decay

`decay` imports a [meson](https://mesonbuild.com/) project into a
[buck2](https://buck2.build/) build, so a project can move onto buck2
without hand-porting its `meson.build` files or losing the configuration
choices they expose.

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

- **Unsatisfiable constraint.** This could be removed, we need to research if
  select_incompatible is a better option, the message could just be
  unsatisfiable or a custom generated one if we have the data to back it up.

- **Fix glib_debug.** See the example/decay.toml file for context, it should work
  to pin this option.

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

- **Formatting.** If a literal inside a select array e.g. `"cpu[x86_64]": [":dep"]` can go in
  the same line instead of a new one, confirm `buildifier` doesn't change it, should reduce
  LoC.

- **Third party dir clean.** If the third_party_dir has folders and files that are not automatically
  generated by decay buckify should be removed, it should reflect exactly decay.toml when generated.

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
  `decay buckify` on `example/` and diffs the generated tree against a
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

- **A `#mesondefine NAME` whose name is never `.set()` in any reachable
  configuration passes through a template untouched.** `config_header_cmd`'s
  `complete_defines` (`decay_buck2/src/lib.rs`) only emits an
  `/* #undef NAME */` sed rule for a name that appears as a `Define` variant
  somewhere — but pinning an option can make the only `.set()` for a name
  statically dead, and `decay_meson_eval` does not evaluate expressions
  under an unsatisfiable path condition, so that `.set()` never runs and the
  name never becomes a `Define` at all. glib's `glibconfig.h.in` hits this
  once `default_library` is pinned away from `static`/`both`:
  `GLIB_STATIC_COMPILATION` and friends are only ever `.set()` inside `if
  glib_build_static_only`, which becomes dead, so `#mesondefine
  GLIB_STATIC_COMPILATION` survives into `glibconfig.h` verbatim and gcc
  rejects it as an invalid preprocessing directive. Real meson still
  substitutes it, because it substitutes a template against the names in
  `configuration_data()` regardless of which C code path is reachable.
  Fixing it needs the full set of `#mesondefine` names a template
  references — from scanning the template file's text, since decay does not
  otherwise know a name it was never told to `.set()` — not just the names
  that got a live `Define` variant.
