//! Control flow that only some configurations take — a `break`, `continue`
//! or `subdir_done()` under an `if` — must drop exactly those configurations
//! from what follows, and nothing else.

use {
    decay_meson_ast::{
        Block,
        ProjectOptions, //
    },
    decay_meson_eval::{
        Sources,
        obj::Machine,
        oracle::{
            Oracle,
            Pinned, //
        },
    },
    std::path::{
        Path,
        PathBuf, //
    },
};

struct TestSources;

impl Sources for TestSources {
    fn build(&self, path: &Path) -> eyre::Result<Block> {
        decay_meson_parse::parse_build(path)
    }

    fn options(&self, dir: &Path) -> eyre::Result<Option<ProjectOptions>> {
        decay_meson_parse::parse_project_options(dir)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read(&self, path: &Path) -> eyre::Result<String> {
        Ok(std::fs::read_to_string(path)?)
    }

    fn list_dir(&self, _dir: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
}

struct TestOracle;

impl Oracle for TestOracle {
    fn option(&self, _name: &str) -> Option<Pinned> {
        None
    }

    fn machine(&self, _machine: Machine, _property: &str) -> Option<String> {
        None
    }

    fn systems(&self) -> Vec<String> {
        [
            "linux",
            "freebsd",
            "netbsd",
            "openbsd",
            "darwin",
            "windows",
            "sunos",
            "android",
            "fuchsia",
            "illumos",
            "haiku",
            "cygwin",
            "aix",
            "hpux",
            "dragonfly",
            "gnu",
            "hurd",
            "qnx",
            "vms",
            "zos",
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

use {
    decay_build_ir::Graph,
    decay_meson_logic::{
        Logic,
        Z3Solver, //
    },
};

fn eval(name: &str, build: &str) -> eyre::Result<(Graph, Logic<Z3Solver>)> {
    let root =
        std::env::temp_dir().join(format!("decay-control-flow-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("meson.build"), build).unwrap();
    std::fs::write(root.join("main.c"), "int main(void) { return 0; }\n").unwrap();
    let r = decay_meson_eval::eval(&TestOracle, &TestSources, &root);
    std::fs::remove_dir_all(&root).ok();
    r
}

/// Which of `linux`/`windows` the target named `name` exists on.
fn systems_of(graph: &Graph, logic: &mut Logic<Z3Solver>, name: &str) -> Vec<&'static str> {
    let target = graph
        .targets
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no target `{name}`"));
    let var = logic
        .arena()
        .var_id("machine:host:system")
        .expect("the build reads host_machine.system()");
    let mut out = Vec::new();
    for system in ["linux", "windows"] {
        let choice = logic.var(var).choice_index(system).unwrap();
        let lit = logic.lit(var, choice);
        let cond = logic.and(target.cond, lit);
        if logic.is_sat(cond) {
            out.push(system);
        }
    }
    out
}

#[test]
fn conditional_break_keeps_the_rest_of_the_iteration_for_everyone_else() {
    let (graph, mut logic) = eval(
        "break",
        r#"
project('t', 'c')
foreach x : ['one', 'two']
  if host_machine.system() == 'linux'
    break
  endif
  executable('exe-' + x, 'main.c')
endforeach
"#,
    )
    .unwrap();
    assert_eq!(systems_of(&graph, &mut logic, "exe-one"), ["windows"]);
    assert_eq!(systems_of(&graph, &mut logic, "exe-two"), ["windows"]);
}

#[test]
fn conditional_break_after_work_stops_later_iterations_only() {
    let (graph, mut logic) = eval(
        "break-late",
        r#"
project('t', 'c')
foreach x : ['one', 'two']
  executable('exe-' + x, 'main.c')
  if host_machine.system() == 'linux'
    break
  endif
endforeach
"#,
    )
    .unwrap();
    assert_eq!(
        systems_of(&graph, &mut logic, "exe-one"),
        ["linux", "windows"]
    );
    assert_eq!(systems_of(&graph, &mut logic, "exe-two"), ["windows"]);
}

#[test]
fn conditional_continue_skips_only_its_own_iteration() {
    let (graph, mut logic) = eval(
        "continue",
        r#"
project('t', 'c')
foreach x : ['one', 'two']
  if x == 'one' and host_machine.system() == 'linux'
    continue
  endif
  executable('exe-' + x, 'main.c')
endforeach
"#,
    )
    .unwrap();
    assert_eq!(systems_of(&graph, &mut logic, "exe-one"), ["windows"]);
    assert_eq!(
        systems_of(&graph, &mut logic, "exe-two"),
        ["linux", "windows"]
    );
}

#[test]
fn subdir_done_in_one_arm_still_runs_the_else() {
    let (graph, mut logic) = eval(
        "subdir-done",
        r#"
project('t', 'c')
if host_machine.system() == 'linux'
  subdir_done()
else
  executable('else-branch', 'main.c')
endif
executable('after', 'main.c')
"#,
    )
    .unwrap();
    assert_eq!(systems_of(&graph, &mut logic, "else-branch"), ["windows"]);
    assert_eq!(systems_of(&graph, &mut logic, "after"), ["windows"]);
}

#[test]
fn subdir_done_inside_a_loop_ends_the_file_for_those_configurations() {
    let (graph, mut logic) = eval(
        "subdir-done-loop",
        r#"
project('t', 'c')
foreach x : ['one', 'two']
  if x == 'two' and host_machine.system() == 'linux'
    subdir_done()
  endif
  executable('exe-' + x, 'main.c')
endforeach
executable('after', 'main.c')
"#,
    )
    .unwrap();
    assert_eq!(
        systems_of(&graph, &mut logic, "exe-one"),
        ["linux", "windows"]
    );
    assert_eq!(systems_of(&graph, &mut logic, "exe-two"), ["windows"]);
    assert_eq!(systems_of(&graph, &mut logic, "after"), ["windows"]);
}

#[test]
fn an_error_every_configuration_reaches_fails_the_import() {
    let err = eval(
        "error",
        r#"
project('t', 'c')
error('unsupported')
executable('x', 'main.c')
"#,
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains("unsupported"), "{err:?}");
}

#[test]
fn an_error_some_configurations_reach_only_removes_those() {
    let (graph, mut logic) = eval(
        "partial-error",
        r#"
project('t', 'c')
if host_machine.system() == 'linux'
  error('no linux')
endif
executable('x', 'main.c')
"#,
    )
    .unwrap();
    assert_eq!(systems_of(&graph, &mut logic, "x"), ["windows"]);
}
