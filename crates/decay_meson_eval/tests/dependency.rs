//! `dependency()`'s candidate list is variational: a name can differ by
//! configuration, and an entry can be present in only some. Each
//! configuration must try exactly the names it has, in its own order, and
//! nothing named only elsewhere.

use {
    decay_build_ir::Graph,
    decay_meson_ast::{
        Block,
        ProjectOptions, //
    },
    decay_meson_eval::{
        Sources,
        obj::Machine,
        oracle::{
            Oracle,
            Pinned,
            Probe, //
        },
    },
    decay_meson_logic::{
        Logic,
        Pc,
        Z3Solver, //
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

/// `found` lists the dependencies something provides, everywhere.
struct TestOracle {
    found: Vec<&'static str>,
}

impl Oracle for TestOracle {
    fn option(&self, _name: &str) -> Option<Pinned> {
        None
    }

    fn machine(&self, _machine: Machine, _property: &str) -> Option<String> {
        None
    }

    fn systems(&self) -> Vec<String> {
        ["linux", "freebsd"].map(str::to_owned).to_vec()
    }

    fn dependency_found(&self, name: &str) -> Option<Probe> {
        self.found.contains(&name).then_some(Probe::Fixed(true))
    }
}

fn eval(name: &str, found: &[&'static str], build: &str) -> eyre::Result<(Graph, Logic<Z3Solver>)> {
    let root = std::env::temp_dir().join(format!("decay-dependency-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.c"), "int main(void) { return 0; }\n").unwrap();
    std::fs::write(root.join("meson.build"), build).unwrap();
    let oracle = TestOracle {
        found: found.to_vec(),
    };
    let r = decay_meson_eval::eval(&oracle, &TestSources, &root);
    std::fs::remove_dir_all(&root).ok();
    r
}

fn target_cond(graph: &Graph, name: &str) -> Pc {
    graph
        .targets
        .iter()
        .find(|t| t.name == name)
        .map_or(Pc::FALSE, |t| t.cond)
}

/// The systems `cond` holds on, among the ones still possible.
fn systems(logic: &mut Logic<Z3Solver>, cond: Pc) -> Vec<&'static str> {
    let var = logic.arena().var_id("machine:host:system").unwrap();
    let mut out = Vec::new();
    for system in ["linux", "freebsd"] {
        let choice = logic.var(var).choice_index(system).unwrap();
        let lit = logic.lit(var, choice);
        let both = logic.and(cond, lit);
        if logic.is_sat(both) {
            out.push(system);
        }
    }
    out
}

#[test]
fn a_name_that_differs_by_system_is_only_tried_where_it_is_named() {
    let (graph, mut logic) = eval(
        "varying",
        &["b"],
        r#"
project('t', 'c')
d = dependency(host_machine.system() == 'linux' ? 'a' : 'b', required: false)
if d.found()
  executable('uses', 'main.c')
endif
"#,
    )
    .unwrap();
    let cond = target_cond(&graph, "uses");
    assert_eq!(
        systems(&mut logic, cond),
        ["freebsd"],
        "`b` is never named on linux"
    );
}

#[test]
fn a_required_varying_name_rules_out_where_it_is_missing() {
    let (graph, mut logic) = eval(
        "required",
        &["a"],
        r#"
project('t', 'c')
d = dependency(host_machine.system() == 'linux' ? 'a' : 'b')
executable('uses', 'main.c', dependencies: d)
"#,
    )
    .unwrap();
    let cond = target_cond(&graph, "uses");
    assert_eq!(
        systems(&mut logic, cond),
        ["linux"],
        "freebsd needs `b`, which nothing provides"
    );
    assert_eq!(systems(&mut logic, Pc::TRUE), ["linux"]);
}

#[test]
fn a_varying_name_nothing_provides_anywhere_is_an_error() {
    let err = eval(
        "nowhere",
        &[],
        r#"
project('t', 'c')
dependency(host_machine.system() == 'linux' ? 'a' : 'b')
"#,
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains("dependency("), "{err:?}");
}

#[test]
fn a_candidate_added_in_one_configuration_is_only_tried_there() {
    let (graph, mut logic) = eval(
        "added",
        &["y"],
        r#"
project('t', 'c')
names = ['x']
if host_machine.system() == 'linux'
  names += 'y'
endif
if dependency(names, required: false).found()
  executable('uses', 'main.c')
endif
"#,
    )
    .unwrap();
    let cond = target_cond(&graph, "uses");
    assert_eq!(systems(&mut logic, cond), ["linux"]);
}

#[test]
fn each_configuration_tries_its_own_order() {
    let (graph, mut logic) = eval(
        "order",
        &["a", "b"],
        r#"
project('t', 'c')
names = host_machine.system() == 'linux' ? ['a', 'b'] : ['b', 'a']
d = dependency(names)
if d.name() == 'a'
  executable('got_a', 'main.c')
endif
if d.name() == 'b'
  executable('got_b', 'main.c')
endif
"#,
    )
    .unwrap();
    let a = target_cond(&graph, "got_a");
    assert_eq!(systems(&mut logic, a), ["linux"]);
    let b = target_cond(&graph, "got_b");
    assert_eq!(systems(&mut logic, b), ["freebsd"]);
}

#[test]
fn not_found_is_the_last_candidate_named_in_each_configuration() {
    let (graph, mut logic) = eval(
        "stub",
        &[],
        r#"
project('t', 'c')
names = host_machine.system() == 'linux' ? ['a', 'b'] : ['c']
d = dependency(names, required: false)
if d.name() == 'b'
  executable('last_b', 'main.c')
endif
if d.name() == 'c'
  executable('last_c', 'main.c')
endif
if d.name() == 'a'
  executable('never', 'main.c')
endif
"#,
    )
    .unwrap();
    let b = target_cond(&graph, "last_b");
    assert_eq!(systems(&mut logic, b), ["linux"]);
    let c = target_cond(&graph, "last_c");
    assert_eq!(systems(&mut logic, c), ["freebsd"]);
    let never = target_cond(&graph, "never");
    assert!(systems(&mut logic, never).is_empty());
}
