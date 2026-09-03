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
