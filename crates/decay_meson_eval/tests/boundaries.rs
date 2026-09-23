//! What crosses a boundary — a path leaving the project, a header dir
//! covering an explicitly listed header, a sibling project's provide — keeps
//! its meaning, or is refused.

use {
    decay_build_ir::{Graph, Source},
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
        Formula,
        Logic,
        Pc,
        VarKind,
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

    fn list_dir(&self, dir: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| PathBuf::from(e.file_name()))
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}

/// Answers `dependency(name)` from `provides`, the way decay's own oracle
/// answers from an earlier project's exported condition.
#[derive(Default)]
struct TestOracle {
    provides: Vec<(String, Formula)>,
}

impl Oracle for TestOracle {
    fn option(&self, _name: &str) -> Option<Pinned> {
        None
    }

    fn machine(&self, _machine: Machine, _property: &str) -> Option<String> {
        None
    }

    fn systems(&self) -> Vec<String> {
        ["linux", "windows", "darwin"].map(str::to_owned).to_vec()
    }

    fn dependency_found(&self, name: &str) -> Option<Probe> {
        self.provides
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| Probe::Formula(f.clone()))
    }
}

/// A scratch project: `files` as `(relative path, contents)`.
fn eval(
    name: &str,
    oracle: &TestOracle,
    files: &[(&str, &str)],
) -> eyre::Result<(Graph, Logic<Z3Solver>)> {
    let root = std::env::temp_dir().join(format!("decay-boundaries-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let r = decay_meson_eval::eval(oracle, &TestSources, &root);
    std::fs::remove_dir_all(&root).ok();
    r
}

/// Which systems `cond` holds on.
fn systems(logic: &mut Logic<Z3Solver>, cond: Pc) -> Vec<&'static str> {
    let var = logic
        .arena()
        .var_id("machine:host:system")
        .expect("the build reads host_machine.system()");
    let mut out = Vec::new();
    for system in ["linux", "windows", "darwin"] {
        let choice = logic.var(var).choice_index(system).unwrap();
        let lit = logic.lit(var, choice);
        let both = logic.and(cond, lit);
        if logic.is_sat(both) {
            out.push(system);
        }
    }
    out
}

fn target_cond(graph: &Graph, name: &str) -> Pc {
    graph
        .targets
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no target `{name}`"))
        .cond
}

const MAIN: (&str, &str) = ("main.c", "int main(void) { return 0; }\n");

#[test]
fn include_dir_lists_a_header_the_explicit_list_only_names_on_some_systems() {
    let (graph, mut logic) = eval(
        "headers",
        &TestOracle::default(),
        &[
            MAIN,
            ("inc/a.h", ""),
            (
                "meson.build",
                r#"
project('t', 'c')
hdrs = []
if host_machine.system() == 'linux'
  hdrs += 'inc/a.h'
endif
executable('exe', 'main.c', hdrs, include_directories: include_directories('inc'))
"#,
            ),
        ],
    )
    .unwrap();
    let target = graph.targets.iter().find(|t| t.name == "exe").unwrap();
    let entries: Vec<Pc> = target
        .attrs
        .headers
        .variants()
        .iter()
        .filter(|h| matches!(&h.value, Source::File(p) if p == Path::new("inc/a.h")))
        .map(|h| h.cond)
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "one entry per header, not one per listing"
    );
    assert_eq!(
        systems(&mut logic, entries[0]),
        ["linux", "windows", "darwin"]
    );
}

#[test]
fn a_path_climbing_out_of_the_project_is_refused() {
    let err = eval(
        "escape",
        &TestOracle::default(),
        &[
            MAIN,
            (
                "meson.build",
                "project('t', 'c')\nexecutable('exe', '../main.c')\n",
            ),
        ],
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("points outside the project"),
        "{err:#}"
    );
}

#[test]
fn an_absolute_include_dir_is_refused() {
    let err = eval(
        "absolute",
        &TestOracle::default(),
        &[
            MAIN,
            (
                "meson.build",
                "project('t', 'c')\ninc = include_directories('/usr/include')\n",
            ),
        ],
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("points outside the project"),
        "{err:#}"
    );
}

#[test]
fn an_absolute_program_path_is_just_not_in_tree() {
    eval(
        "program",
        &TestOracle::default(),
        &[
            MAIN,
            (
                "meson.build",
                "project('t', 'c')\nfind_program('/bin/sh', required: false)\n",
            ),
        ],
    )
    .unwrap();
}

#[test]
fn a_sibling_provided_on_some_systems_is_found_only_there() {
    // The provider: its root `declare_dependency()` exists only on linux, and
    // only when its own `extra` option is on.
    let (mut graph, mut logic) = eval(
        "provider",
        &TestOracle::default(),
        &[
            (
                "meson_options.txt",
                "option('extra', type: 'boolean', value: true)\n",
            ),
            (
                "meson.build",
                r#"
project('sib', 'c')
if get_option('extra') and host_machine.system() == 'linux'
  sib_dep = declare_dependency()
endif
"#,
            ),
        ],
    )
    .unwrap();
    let provide = graph.provides.iter_mut().find(|p| p.name == "sib").unwrap();
    let (found, dropped) = logic
        .arena_mut()
        .export(provide.cond, |var| var.kind != VarKind::Option);
    assert_eq!(
        dropped,
        ["option:extra"],
        "the provider's own option stays behind"
    );

    // The consumer sees the provider's system condition, not "everywhere".
    let oracle = TestOracle {
        provides: vec![("sib".to_owned(), found)],
    };
    let (graph, mut logic) = eval(
        "consumer",
        &oracle,
        &[
            MAIN,
            (
                "meson.build",
                r#"
project('t', 'c')
d = dependency('sib', required: false)
if d.found()
  executable('uses', 'main.c')
endif
"#,
            ),
        ],
    )
    .unwrap();
    let cond = target_cond(&graph, "uses");
    assert_eq!(systems(&mut logic, cond), ["linux"]);
}

#[test]
fn a_host_path_probe_is_not_there_for_the_build() {
    let (graph, _) = eval(
        "fs-host",
        &TestOracle::default(),
        &[
            MAIN,
            (
                "meson.build",
                r#"
project('t', 'c')
fs = import('fs')
if fs.is_dir('/usr') or fs.exists('../outside')
  executable('never', 'main.c')
endif
"#,
            ),
        ],
    )
    .unwrap();
    assert!(graph.targets.iter().all(|t| t.name != "never"));
}
