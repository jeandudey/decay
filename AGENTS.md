# decay

`decay` imports a [meson](https://mesonbuild.com/) project into a
[buck2](https://buck2.build/) build, so a project can move onto buck2
without hand-porting its `meson.build` files or losing the configuration
choices they expose.

## Rules

- Always materialize git lfs (`git lfs pull`) when working, no skip before doingo
  anything, otherwise build error.
- **"Known gaps" below is a live list of what's still missing, not a
  changelog.** When a gap is fully fixed, delete its entry outright. When a
  gap is partially fixed, rewrite the entry down to only what's still open —
  never append a "Landed:"/"Fixed:" paragraph narrating how or when it got
  fixed, what it took, or what got verified. Git history and commit messages
  are where that belongs; this file is where a person checks what to expect
  before they hit it.

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
- **Import GTK4 end to end.** `example/` doubles as decay's own dependency
  smoke test, growing one real project at a time until GTK4 itself
  `buck2 build`s from it — see "Support all of meson wrapdb" below for the
  running list of what that still needs.

See `example/` for a libepoxy import, and `example/decay.toml` for how a
project's escape hatches (`[systems]`, `[probes]`, `[programs]`,
`[dependencies]`) are meant to read.

## Known gaps

- **Wrap support.** `[[project]] wrap = "name"` resolves both `[wrap-file]`
  and `[wrap-git]` wraps, including a `patch_directory` overlay fetched
  alongside the project's own archive when something actually references an
  overlaid file (`src/wrapdb.rs`, `src/wrap_cache.rs`). Still open:
  - The legacy `patch_filename` archive overlay is refused (`wrapdb::parse_wrap`
    bails with the key to add) — no current wrapdb release uses it.
  - `[provide] dependency_names` is not read — a wrap resolves against a
    sibling `dependency()` only through the same name-matching heuristic as
    any other project (see "`declare_dependency()` provide heuristic is
    narrow" below), not its own declared provides.

- **Unsatisfiable constraint.** This could be removed, we need to research if
  select_incompatible is a better option, the message could just be
  unsatisfiable or a custom generated one if we have the data to back it up.

- **has_function.** Built from an authoritative source (zig's `abilists`,
  `decay_zig`), never hand-curated — landed for `linux`/`freebsd`/`netbsd`,
  each a settled `select()` over `os`/`abi`/`cpu`, not an open knob. Still
  open: `windows`/`darwin`/others leave it an open knob (no equally
  authoritative, automatically-parseable source found yet); `has_header` and
  the rest are a harder follow-up (header presence also depends on optional
  dev packages, not just the libc). Known rough edge: a symbol real on
  `linux` (both abis) + `freebsd` but not `netbsd` can misplace onto an
  `abi[musl]` arm (the solver allows an axis var to take "no value") —
  harmless on `abi[gnu]`, and no `example/` platform exercises `abi[musl]`.

- **Compile/link probes resolved by `zig cc` at import time.** `cc.has_header`,
  `cc.check_header`, `cc.has_type`, `cc.has_header_symbol`, `cc.has_member`,
  `cc.compiles`, `cc.links` and `cc.symbols_have_underscore_prefix` are built
  with `zig cc -target <triple>` per `(os, cpu, abi)` in decay's configured
  matrix (`src/probe.rs`, `oracle::Probe::Matrix`), linux-gnu pinned to the
  newest glibc zig ships; built everywhere → plain `true`, nowhere → dead
  branch, on some → a real `select()`, never a synthetic `has_foo_bar`
  constraint. A `prefix:`/code/`args:` that differs by configuration is built
  once per region it is constant in (`compile_probes` in
  `decay_meson_eval/src/methods.rs`). An explicit `decay.toml [probes]`
  answer still wins first. Systems zig cannot probe end to end (`darwin`,
  `windows`, `illumos`, `android`, `fuchsia`) are meant to be left out of
  `[systems]` entirely.

  Still in scope:
  - **`dependencies:` other than `threads` or a system library** — a
    `pkg-config` or sibling dependency's include dirs and flags are not
    replayed on the `zig cc` line, so the probe stays an open knob.
  - **C++ probes** — `cpp.has_header()` and kin are built as C.
  - **compile-time `cc.sizeof` of a *type*** — the `static_assert` binary
    search meson falls back to when it cannot run. Goes through
    `Oracle::type_size`, not `compile_probe`.
  - **gcc-only builtins.** `-fgnuc-version=` fixes the version-guard class
    of probe, not one that uses a GCC extension clang never implemented
    (graphene's `__builtin_shuffle`, no `GRAPHENE_HAS_GCC`). Needs a
    `compiler` axis on `Probe::Matrix`, or a `[probes]` override.

  Deferred, each its own follow-up:
  - **`cc.has_argument()`/`has_link_argument()`/`has_multi_arguments()` are
    compiler-specific** — `zig cc` is clang, so "does `-Wfoo` exist" can
    diverge from a gcc toolchain. The handful of known gcc/clang-only
    warning flags are gated (`GCC_ONLY_WARNING_ARGS`/`CLANG_ONLY_WARNING_ARGS`
    in `decay_meson_eval/src/methods.rs` — extend as projects turn up more);
    the general case wants a real per-toolchain answer.
  - **`cc.run()` proper, `cc.alignment()`'s value, `cc.compute_int()`** need
    the probe *executed*, not just linked — decay does not cross-run. Stay
    on the `decay.toml` path (see `cc.compute_int()`'s own entry below).
  - **Kernel/libc-header-vintage probes** (`HAVE_FUTEX_TIME64` and kin)
    resolve against `zig`'s *bundled* headers — a version pin, not a real
    per-host fact (see "An unanswered probe defaults to `true`" below).
  - **Determinism / golden tree** — `example/`'s golden tree shifts whenever
    the pinned `zig`'s libc or headers change.

- **No MSVC.** The `compiler` constraint is `gcc`/`clang` only, `[compilers]`
  refuses `msvc`, and `windows` means mingw: every probe is answered by
  `zig cc`, a gcc-compatible driver. Needs real clang-cl or MSVC probing
  before it can come back.

- **Python3 genrules.** the genrules using python should try to use python rules if possible
  to define the scripts, and only fallback to genrule with an override in decay.toml if it
  turns out it doesn't work with buck2 rules, also python3 should point to an hermetic
  python3 executable as defined in toolchains//.

- **glib's `tests` option stays off.** Enabling it means verifying all
  ~367 of glib's test targets, not just spot-checking a couple — its own
  follow-up.

- **`declare_dependency(sources: [...])` with compilable sources.** Real
  headers vs. compilable translation units are split and spliced into a
  consumer's own `srcs` (`fn_declare_dependency`, `copylib_source_groups` in
  `decay_buck2`). Still not modelled: `declare_dependency(objects:)` (glib's
  `libglib_static_dep`).

- **Configuration-dependent install paths and `.pc` variables.**
  `Attrs.install_dir` is `Option<String>` and `src/packages.rs`'s `Package.
  variables` is a flat `Vec`. So a `configure_file()` whose `output:` or
  `install_dir:` varies by configuration (glib's systemtap `.stp`, keyed on
  `cpu_family`) aborts with "expected a single string … N variants", and
  `single_valued_pairs` silently drops any `pkg.generate()` /
  `declare_dependency()` variable that came out configuration-dependent
  (glib's `multiarch`-keyed `giomoduledir`) rather than emitting a
  `select()`. Both want the value carried variationally through to emit.

- **`link_args:`/`c_args:` embedding a path to a same-project file.**
  Resolved to a real reference (`Flag::File`) when the flag's own trailing
  word names a file the project's own graph provides (pcre2/zlib/libglvnd
  version scripts). Still narrow on purpose: only the flag's own trailing
  word is checked, not an arbitrary embedded substring, and an ambiguous
  match — two targets declaring the same output name — is left as a plain
  literal rather than disambiguated.

- **`windows.compile_resources` / `fs.copyfile` are minimal.**
  `compile_resources` drops `args:` (resource-compiler flags) and
  `include_directories:` (RC search paths); `fs.copyfile` emits a `cp`
  command, which a Windows genrule does not have. Both matter for the
  Windows target that is a priority. A `.rc` source is split into its own
  `windows_resource` target, but still `srcs`-only: one that `#include`s a
  project header needs `include_directories`/`headers` emitted on that rule
  too.

- **The `python` module is a stub.** Only `import('python').
  find_installation()` (resolved like any `[programs]` entry) and
  `.language_version()` (fabricated `"3.12"`, the way `cc.version()` is).
  A project that builds Python extension modules needs `.extension_module()`,
  `.dependency()`, `.install_sources()`, `.get_install_dir()`,
  `.get_variable()`, none of which exist.

- **`cc.compute_int()` has no configured answer.** It falls back to the
  call's `guess:` and errors without one. Like `[sizeof]` / `[alignment]`,
  the no-guess case should be answerable from `decay.toml`.

- **glib: `girepository-2.0` and the module-loading `gio` variant are
  untried.** Also, `gdbus-daemon-generated`/`xdp-dbus` declare two outputs
  (`.h` + `.c`); decay's single-`out` genrule keeps only the `.h`, which
  those two (and the gio tools) may need.

- **`run_command()` is answered deterministically or refused.** Three ways,
  in priority order (`ConfigOracle::run_command`, `src/run_command.rs`): a
  per-project `decay.toml` `commands` table; `git describe` synthesized from
  the pinned ref; a read-only allowlist executor
  (`cat`/`head`/`tail`/`echo`/`true`/`false`, path args must stay inside the
  checkout). Anything else is a hard error naming the `commands` key to add.
  Still open: config-dependent (`select()`-keyed) output is not modelled;
  the allowlist is a fixed set.

- **`declare_dependency()` provide heuristic is narrow.** A sibling
  `dependency('x')` resolves against a `pkg.generate()`/`declare_dependency()`
  in project `x` only when `Packages::register` can match it by name: the
  project's own `short_name`, a `pkg.generate(filebase:)`/`name:`, or an
  explicit `meson.override_dependency(name, dep)` (which always wins over a
  same-name `pkg.generate()` collision, regardless of which ran first during
  evaluation — `Interp::dependency_overrides`, merged in `Interp::finish()`).
  A wrap whose `[provide] dependency_names` differs from the directory name,
  or a project with several root `declare_dependency()` calls and no
  override naming the right one, still would not resolve.

  A `pkg.generate(requires: [...])`'s `Requires:` (plain-string entries
  only) is captured and walked transitively (`Packages::targets()`), so
  resolving one name also pulls its `Requires:` into the consumer's `deps` —
  but a `configure_file()`-produced `.pc` still records no `requires`.
  `dependency()`'s returned object reports `type_name() == "internal"` for a
  sibling resolution, matching meson's own distinction from `"pkgconfig"`,
  and `subproject(name).get_variable(key)` answers from a sibling's settled
  top-level variable when it is a single-valued string/bool/int (still
  single-valued only; a configuration-varying answer is unsupported either
  way). A variable that is really a list of dicts each carrying just a
  `name` string — a project's own "what did I end up building" summary
  (cairo's `built_features`, pango's own `subproject('cairo').
  get_variable('built_features')`) — has no automatic answer at all: the
  real value's entries embed `Dep` objects tied to the *other* project's own
  graph, meaningless once evaluation moves on, so decay never evaluates it
  itself. `decay.toml`'s `[subprojects.name]` names, as plain strings, which
  features that project's own configured options mean it actually built
  (`Oracle::subproject_variable_names`) — the only shape reconstructed so
  far, since a consumer has only ever read the `name` field back out as a
  membership test.

- **A provide gated on the provider's own options reads as present.** A
  sibling's `dependency()` sees the provider's presence condition over
  meson's shared variables (system, probes, builtin options), so one only
  built on linux is found only on linux (`Arena::export`, `Probe::Formula`).
  The provider's own project options are not visible to the consumer and are
  quantified out: a `pkg.generate()` behind an open `option('x')` counts as
  present under every setting (decay warns). Carrying them needs the
  consumer to select on the provider's own constraint labels.

- **An unanswered probe defaults to `true`.** `probe_var()`
  (`decay_meson_eval/src/lib.rs`) gives every `VarKind::Probe` constraint a
  hardcoded `default = 0` ("true"). Most probes no longer reach it (see
  "Compile/link probes resolved by `zig cc`" above); what still does is
  `cc.run()` and a probe with `dependencies:` decay cannot replay. The zig
  answer itself is a libc *vintage* pin, not a per-host fact: linux-gnu
  probes see the newest glibc zig ships, so glib's `HAVE_FUTEX_TIME64` and
  `res_ndestroy()` read present and fail on an older host. `example/`
  settles both off in `[probes]`. Fixing it for real wants a glibc floor in
  `decay.toml` that the probes pin to instead.

- **Better diagnostics.** If something fails to import because it needs user input
  we should provide a way for the user to fix it if possible. If it is something
  we don't have implemented then we should provide that. Ideally we should collect
  unimplemented functions and methods in meson and either provide these in the
  program to let the user know it hasn't been implemented, and also to keep a
  list here in known gaps.

- **Adding meson specific buck2 rules.** A config-header template
  (`#mesondefine`) is emitted as a `genrule` shelling out to `sed`, not a
  native buck2 rule — the emitted `BUCK` should read like a config-header
  rule a person would reach for.

- **add_test_setup.** Matched now, but a no-op stub (`warn_unsupported()`
  then `Value::Unset`) — the warning spam is gone, but the test-setup data
  (env, wrapper) still goes nowhere. Needs modelling against buck2's own
  test-env support, not just silencing.

- **Windows OS libraries the link probe misses read as not found.** A
  `cc.find_library()`/`dependency()` nothing confirms is not found (optional)
  or an error (required), never a knob. mingw ships no `libruntimeobject.a`,
  so `runtimeobject` and kin need a mingw-w64 `.def`-name source (same shape
  as the glibc `abilists` read) or a `[system_libraries]` entry.

- **`cc.preprocess()` only handles a single, unconditional source.** The
  common case is a real genrule that shells out to the real C preprocessor
  (`cc -E -P -x c`, plus the call's `include_directories:` and the same
  generated-header broadcast a compiled target gets for free — `preprocess_cmd`
  in `decay_buck2`; construction is `fn_cc_preprocess` in
  `decay_meson_eval/src/builtins.rs`): fontconfig's `fcobjshash.gperf.h`
  (needs its `#include "fcobjs.h"` actually expanded for gperf to see
  keywords) and libffi's `libffi.map` (a linker version-script) both build
  from it now. Several sources, or one only present in some configurations
  (libffi's per-arch `foreach`-built MSVC assembly list), still fall back to
  the old passthrough — fine where nothing reads the macro-expanded result,
  wrong where it does. `compile_args:`/`dependencies:` on the call are not
  read either.

- **Support all of meson wrapdb.** This should be the biggest showcase and
  smoke test for decay — currently exercises 15 of wrapdb's ~250+ projects in
  `example/decay.toml` (`zlib`, `bzip2`, `libpng`, `google-brotli`, `pcre2`,
  `libxext`, `libffi`, `fribidi`, `graphite2`, `pixman`, `cairo`, `freetype2`,
  `harfbuzz`, `fontconfig`, `pango`).

  **GTK4 end-to-end — what's still missing.** Checked against gtk's own
  `meson.build` (`dependency()` calls, tag `4.22.4`) to turn "try the
  dependency graph" into a concrete list. Already imported: `glib`/
  `gobject`/`gio`/`gmodule` (as `glib`), `epoxy`, `graphene`, `xorgproto`,
  `libxext`, `fribidi`, `graphite2`, `pixman`, `cairo` (freetype + fontconfig
  font backends and `cairo-gobject` wired in, since both were already
  imported; still not the `xlib`/`xcb`/`png` surface backends — `xlib`/`xcb`
  need real X11, `png` needs libpng wired in too, next), `fontconfig`
  (`buck2 build`s end to end), `freetype2` (zlib + bzip2 + libpng + brotli
  support; `harfbuzz` stays off in freetype2 itself — harfbuzz's own
  `dependency('freetype2', ..., default_options: ['harfbuzz=disabled'])` is
  how upstream avoids the cycle, and decay resolves that sibling dependency
  against freetype2 as already configured rather than re-running it),
  `harfbuzz` (+ its bundled `harfbuzz-subset`; freetype2, graphite2,
  glib/gobject, and cairo (`harfbuzz-cairo`) integration all wired in since
  every one was already imported — `chafa`/`icu` stay off since nothing here
  provides either, and its `utilities` CLI tools stay off as out of scope),
  `pango` (+ `pangocairo`, `pangoft2`; a git checkout pinned to `1.58.2`, not
  wrapdb's `pango` wrap — its latest release, 1.56.4, calls fontconfig's
  `FcFreeTypeQueryAll()` without the `#include` current fontconfig split it
  behind, unguarded under the `-Werror=implicit` pango's own meson.build
  always turns on; fixed upstream after 1.56.4, first released in 1.58.2.
  `libthai`/`xft` stay off since nothing here provides either, and
  `introspection`/docs/tests/examples are out of scope).
  Still needed, in roughly the order a next attempt should reach for them:
  - `gdk-pixbuf-2.0` — blocked today on the `dependency()`
    configuration-varying-name rewrite (see "`declare_dependency()` provide
    heuristic is narrow" above); GTK4 also wants at least one of its loader
    backends (`libpng`/`libtiff-4`/`libjpeg`).
  - `xkbcommon` — required whenever the Wayland backend is enabled.
  - The remaining X11 extension libraries GTK4's X11 backend links against
    directly: `xrandr`, `xrender`, `xi`, `xcursor`, `xdamage`, `xfixes`,
    `xinerama` — same shape as the already-imported `libxext`/`xorgproto`,
    likely each its own small wrapdb or system entry.
  - `libdrm` (Linux only) and, further out, the optional pieces GTK4 can
    build without (`gobject-introspection`, `iso-codes` — already blocked
    above on `subproject().get_variable()` of a non-decay-imported project,
    `vulkan`, `wayland-client`/`wayland-protocols`/`wayland-egl`,
    `cloudproviders`, `sysprof`, `tracker-sparql`, `accesskit`) — not needed
    for a first GTK4 `buck2 build`, since every one of those is
    `required: false` in gtk's own `meson.build`.

- **Pretty print errors.** Use annotate-snippets crate from rust-lang for this
  current errors are crap.

- **This should not be in each project.**

```
cxx_library(
    name = "threads",
    exported_preprocessor_flags = ["-pthread"],
    exported_linker_flags = ["-pthread"],
    visibility = ["PUBLIC"],
)
```

  The libraries generated by Meson that are common to all projects (only common
  in the sense of builtin ones like threads).
