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
            ImpossibleTarget,
            MatrixSystem,
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

    /// `sizeof`: 8 on linux, 4 on freebsd; a `struct nope` compiles nowhere.
    fn value_probe(&self, probe: &CompileProbe) -> Option<Vec<(Option<i64>, Vec<MatrixSystem>)>> {
        self.asked.borrow_mut().push(probe.clone());
        let on = |system: &str| MatrixSystem {
            system: system.to_owned(),
            axes: Vec::new(),
            rows: vec![Vec::new()],
        };
        if probe.snippet().contains("struct nope") {
            return Some(vec![(None, vec![on("linux"), on("freebsd")])]);
        }
        Some(vec![
            (Some(8), vec![on("linux")]),
            (Some(4), vec![on("freebsd")]),
        ])
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

#[test]
fn sizeof_is_measured_per_system() {
    let oracle = TestOracle::default();
    let (graph, mut logic) = eval(
        "sizeof",
        &oracle,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
if cc.sizeof('long', prefix: '#include <stddef.h>') == 8
  executable('wide', 'main.c')
endif
if cc.sizeof('struct nope') == -1
  executable('nope', 'main.c')
endif
"#,
    );
    assert!(matches!(
        &oracle.asked.borrow()[0].kind,
        CompileProbeKind::Sizeof { name, prefix } if name == "long" && prefix == "#include <stddef.h>"
    ));
    let wide = target_cond(&graph, "wide");
    assert_eq!(systems(&mut logic, wide), ["linux"]);
    let nope = target_cond(&graph, "nope");
    assert_eq!(systems(&mut logic, nope), ["linux", "freebsd"]);
}

/// Builds everywhere except riscv64 on freebsd, which it never builds for.
struct NoRiscvFreebsd;

const CPU: &str = "prelude//cpu/constraints:cpu";

impl Oracle for NoRiscvFreebsd {
    fn option(&self, _name: &str) -> Option<Pinned> {
        None
    }

    fn machine(&self, _machine: Machine, _property: &str) -> Option<String> {
        None
    }

    fn systems(&self) -> Vec<String> {
        ["linux", "freebsd"].map(str::to_owned).to_vec()
    }

    fn compile_probe(&self, _probe: &CompileProbe) -> Option<Probe> {
        let cpus = vec!["riscv64".to_owned(), "x86_64".to_owned()];
        Some(Probe::Matrix(vec![
            MatrixSystem {
                system: "linux".to_owned(),
                axes: Vec::new(),
                rows: vec![Vec::new()],
            },
            MatrixSystem {
                system: "freebsd".to_owned(),
                axes: vec![(CPU.to_owned(), cpus)],
                rows: vec![vec!["x86_64".to_owned()]],
            },
        ]))
    }

    fn impossible_targets(&self) -> Vec<ImpossibleTarget> {
        vec![ImpossibleTarget {
            system: "freebsd".to_owned(),
            setting: CPU.to_owned(),
            domain: vec!["riscv64".to_owned(), "x86_64".to_owned()],
            value: "riscv64".to_owned(),
        }]
    }
}

#[test]
fn an_impossible_target_is_ruled_out() {
    let root = std::env::temp_dir().join(format!("decay-probes-impossible-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.c"), "int main(void) { return 0; }\n").unwrap();
    std::fs::write(
        root.join("meson.build"),
        "project('t', 'c')\nif meson.get_compiler('c').has_header('stdio.h')\n  executable('uses', 'main.c')\nendif\n",
    )
    .unwrap();
    let (graph, mut logic) = decay_meson_eval::eval(&NoRiscvFreebsd, &TestSources, &root).unwrap();
    std::fs::remove_dir_all(&root).ok();
    let cond = target_cond(&graph, "uses");
    let system = logic.arena().var_id("machine:host:system").unwrap();
    let cpu = logic.arena().var_id(&format!("constraint:{CPU}")).unwrap();
    let freebsd = logic.var(system).choice_index("freebsd").unwrap();
    let freebsd = logic.lit(system, freebsd);
    let at = |logic: &mut Logic<Z3Solver>, value: &str| {
        let choice = logic.var(cpu).choice_index(value).unwrap();
        let lit = logic.lit(cpu, choice);
        logic.and(freebsd, lit)
    };
    let riscv = at(&mut logic, "riscv64");
    assert!(!logic.is_sat(riscv), "riscv64 on freebsd is ruled out");
    let x86 = at(&mut logic, "x86_64");
    let missing = logic.not(cond);
    let missing = logic.and(x86, missing);
    assert!(!logic.is_sat(missing), "builds on x86_64 freebsd");
}

fn eval_with(name: &str, oracle: &dyn Oracle, build: &str) -> (Graph, Logic<Z3Solver>) {
    let root = std::env::temp_dir().join(format!("decay-probes-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.c"), "int main(void) { return 0; }\n").unwrap();
    std::fs::write(root.join("meson.build"), build).unwrap();
    let r = decay_meson_eval::eval(oracle, &TestSources, &root);
    std::fs::remove_dir_all(&root).ok();
    r.unwrap()
}

/// libc exports `memcpy` only; the compiler has `__builtin_ctzl` and
/// `__builtin_alloca`. `decay.toml` settles `alloca` absent.
#[derive(Default)]
struct Functions {
    asked: RefCell<Vec<CompileProbe>>,
}

impl Oracle for Functions {
    fn option(&self, _name: &str) -> Option<Pinned> {
        None
    }

    fn machine(&self, _machine: Machine, _property: &str) -> Option<String> {
        None
    }

    fn systems(&self) -> Vec<String> {
        ["linux", "freebsd"].map(str::to_owned).to_vec()
    }

    fn probe(&self, name: &str, what: &str) -> Option<Probe> {
        (name == "has_function").then(|| Probe::Fixed(what == "memcpy"))
    }

    fn probe_configured(&self, name: &str, what: &str) -> bool {
        name == "has_function" && what == "alloca"
    }

    fn compile_probe(&self, probe: &CompileProbe) -> Option<Probe> {
        self.asked.borrow_mut().push(probe.clone());
        let snippet = probe.snippet();
        Some(Probe::Fixed(
            snippet.contains("__has_builtin(__builtin_ctzl)")
                || snippet.contains("__has_builtin(__builtin_alloca)"),
        ))
    }
}

#[test]
fn has_function_falls_back_to_a_compiler_builtin() {
    let oracle = Functions::default();
    let (graph, _) = eval_with(
        "builtin",
        &oracle,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
if cc.has_function('__builtin_ctzl')
  executable('ctzl', 'main.c')
endif
if cc.has_function('memcpy')
  executable('memcpy', 'main.c')
endif
if cc.has_function('alloca')
  executable('alloca', 'main.c')
endif
if cc.has_function('nope')
  executable('nope', 'main.c')
endif
"#,
    );
    let names: Vec<&str> = graph.targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["ctzl", "memcpy"]);

    // Only the two libc misses decay.toml does not settle are built, each as
    // meson's own builtin check.
    let asked = oracle.asked.borrow();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked.iter().all(CompileProbe::links));
    let ctzl = asked[0].snippet();
    assert!(
        ctzl.contains("#if !__has_builtin(__builtin_ctzl)"),
        "{ctzl}"
    );
    assert!(
        ctzl.contains("#if !1 && !defined(__builtin_ctzl) && !1"),
        "{ctzl}"
    );
    let nope = asked[1].snippet();
    assert!(
        nope.contains("#if !__has_builtin(__builtin_nope)"),
        "{nope}"
    );
    assert!(nope.contains("__builtin_nope;"), "{nope}");
}

/// Builds only on arm64, and says `prelude//cpu/constraints:cpu` is how the
/// generated build spells `cpu_family()`.
struct Arm64Only;

impl Oracle for Arm64Only {
    fn option(&self, _name: &str) -> Option<Pinned> {
        None
    }

    fn machine(&self, _machine: Machine, _property: &str) -> Option<String> {
        None
    }

    fn systems(&self) -> Vec<String> {
        vec!["linux".to_owned()]
    }

    fn machine_setting(&self, setting: &str) -> Option<(&'static str, Vec<(String, String)>)> {
        (setting == CPU).then(|| {
            let spelled = [("x86_64", "x86_64"), ("aarch64", "arm64"), ("arm", "arm32")]
                .map(|(f, v)| (f.to_owned(), v.to_owned()))
                .to_vec();
            ("cpu_family", spelled)
        })
    }

    fn compile_probe(&self, _probe: &CompileProbe) -> Option<Probe> {
        Some(Probe::Matrix(vec![MatrixSystem {
            system: "linux".to_owned(),
            axes: vec![(
                CPU.to_owned(),
                vec!["arm64".to_owned(), "x86_64".to_owned()],
            )],
            rows: vec![vec!["arm64".to_owned()]],
        }]))
    }
}

#[test]
fn a_probe_on_the_cpu_asks_cpu_family_itself() {
    let (graph, mut logic) = eval_with(
        "cpu-family",
        &Arm64Only,
        r#"
project('t', 'c')
cc = meson.get_compiler('c')
if host_machine.cpu_family() == 'aarch64'
  if cc.compiles('int y;', name: 'y')
    executable('neon', 'main.c')
  endif
endif
if cc.compiles('int y;', name: 'y')
  executable('y', 'main.c')
endif
"#,
    );
    assert!(
        logic.arena().var_id(&format!("constraint:{CPU}")).is_none(),
        "no second variable for the cpu"
    );
    // On `cpu_family() == 'aarch64'` the probe holds outright.
    let neon = target_cond(&graph, "neon");
    let family = logic.arena().var_id("machine:host:cpu_family").unwrap();
    let aarch64 = logic.var(family).choice_index("aarch64").unwrap();
    let aarch64 = logic.lit(family, aarch64);
    assert_eq!(neon, aarch64);
    let y = target_cond(&graph, "y");
    assert_eq!(y, aarch64);
}
