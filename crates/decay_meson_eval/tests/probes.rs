//! A compiler probe decay can rebuild reaches the oracle as a
//! [`CompileProbe`], once per region its inputs are constant in; only one it
//! cannot rebuild becomes a knob.

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
            CompileProbe,
            CompileProbeKind,
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
    std::{
        cell::RefCell,
        path::{
            Path,
            PathBuf, //
        },
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

/// Records every probe it is asked to build; one "builds" when its source
/// mentions `HAVE_A`. `found` lists the dependencies something provides.
#[derive(Default)]
struct TestOracle {
    asked: RefCell<Vec<CompileProbe>>,
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

    fn compile_probe(&self, probe: &CompileProbe) -> Option<Probe> {
        self.asked.borrow_mut().push(probe.clone());
        Some(Probe::Fixed(probe.snippet().contains("HAVE_A")))
    }

    fn dependency_found(&self, name: &str) -> Option<Probe> {
        self.found.contains(&name).then_some(Probe::Fixed(true))
    }
}

fn eval(name: &str, oracle: &TestOracle, build: &str) -> (Graph, Logic<Z3Solver>) {
    let root = std::env::temp_dir().join(format!("decay-probes-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.c"), "int main(void) { return 0; }\n").unwrap();
    std::fs::write(root.join("HAVE_A.c"), "int HAVE_A;\n").unwrap();
    std::fs::write(root.join("meson.build"), build).unwrap();
    let r = decay_meson_eval::eval(oracle, &TestSources, &root);
    std::fs::remove_dir_all(&root).ok();
    r.unwrap()
}

fn target_cond(graph: &Graph, name: &str) -> Pc {
    graph
        .targets
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no target `{name}`"))
        .cond
}

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
fn a_prefix_that_differs_by_system_is_probed_per_system() {
    let oracle = TestOracle::default();
    let (graph, mut logic) = eval(
        "prefix",
        &oracle,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
pre = ''
if host_machine.system() == 'linux'
  pre += '#define HAVE_A 1\n'
endif
if cc.compiles(pre + 'int x;', name: 'x')
  executable('uses', 'main.c')
endif
"#,
    );
    assert_eq!(oracle.asked.borrow().len(), 2, "one probe per prefix");
    let cond = target_cond(&graph, "uses");
    assert_eq!(systems(&mut logic, cond), ["linux"]);
    assert!(
        logic.arena().var_id("probe:c:compiles:x").is_none(),
        "no knob"
    );
}

#[test]
fn threads_check_header_and_links_are_rebuilt() {
    let oracle = TestOracle::default();
    eval(
        "shapes",
        &oracle,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
cc.has_header('pthread.h', dependencies: dependency('threads'))
cc.check_header('malloc.h')
cc.has_member('fd_set', 'fds_bits', prefix: ['#include <sys/types.h>', '#include <sys/select.h>'])
cc.links('int main(void) { return 0; }', args: '-lrt', name: 'rt')
cc.symbols_have_underscore_prefix()
"#,
    );
    let asked = oracle.asked.borrow();
    let kinds: Vec<&CompileProbeKind> = asked.iter().map(|p| &p.kind).collect();
    assert!(matches!(kinds[0], CompileProbeKind::Header { header, .. } if header == "pthread.h"));
    assert_eq!(asked[0].args, ["-pthread"]);
    assert!(matches!(kinds[1], CompileProbeKind::Header { header, .. } if header == "malloc.h"));
    assert!(matches!(
        kinds[2],
        CompileProbeKind::Member { prefix, .. }
            if prefix == "#include <sys/types.h>\n#include <sys/select.h>"
    ));
    assert!(asked[3].links());
    assert_eq!(asked[3].args, ["-lrt"]);
    assert!(asked[4].snippet().contains("__USER_LABEL_PREFIX__"));
}

#[test]
fn a_file_argument_is_probed_by_its_contents() {
    let oracle = TestOracle::default();
    let (graph, _) = eval(
        "file",
        &oracle,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
if cc.links(files('HAVE_A.c'), name: 'from a file')
  executable('uses', 'main.c')
endif
"#,
    );
    assert_eq!(oracle.asked.borrow()[0].snippet(), "int HAVE_A;\n\n");
    assert!(graph.targets.iter().any(|t| t.name == "uses"));
}

#[test]
fn a_pkg_config_dependency_leaves_the_probe_open() {
    let oracle = TestOracle {
        found: vec!["foo"],
        ..TestOracle::default()
    };
    let (_, logic) = eval(
        "pkgconfig",
        &oracle,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
cc.has_header('foo.h', dependencies: dependency('foo', required: false))
"#,
    );
    assert!(oracle.asked.borrow().is_empty());
    assert!(logic.arena().var_id("probe:c:has_header:foo.h").is_some());
}

#[test]
fn a_dependency_nothing_provides_adds_nothing_to_the_probe() {
    let oracle = TestOracle::default();
    let (_, logic) = eval(
        "unprovided",
        &oracle,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
cc.has_header('foo.h', dependencies: dependency('foo', required: false))
"#,
    );
    assert_eq!(oracle.asked.borrow().len(), 1);
    assert!(logic.arena().var_id("probe:c:has_header:foo.h").is_none());
}
