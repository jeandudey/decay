//! `i18n.merge_file()` becomes the `msgfmt` command meson's own
//! `msgfmthelper` runs, and a `configure_file()` output named through
//! `@BASENAME@`/`@PLAINNAME@` is named after its input.

use {
    decay_build_ir::{
        CmdArg,
        Graph,
        Kind, //
    },
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

/// Supplies `msgfmt`, nothing else.
struct TestOracle;

impl Oracle for TestOracle {
    fn option(&self, _name: &str) -> Option<Pinned> {
        None
    }

    fn machine(&self, _machine: Machine, _property: &str) -> Option<String> {
        None
    }

    fn systems(&self) -> Vec<String> {
        vec!["linux".to_owned()]
    }

    fn has_program(&self, name: &str) -> bool {
        name == "msgfmt"
    }
}

/// Lays out shared-mime-info's shape: the template and ITS rules in `data/`,
/// catalogs in `po/`.
fn eval(name: &str, root_build: &str, data_build: &str) -> eyre::Result<Graph> {
    let root = std::env::temp_dir().join(format!("decay-i18n-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    for dir in ["data/its", "po"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("meson.build"), root_build).unwrap();
    std::fs::write(root.join("data/meson.build"), data_build).unwrap();
    std::fs::write(root.join("data/mime.xml.in"), "<mime-info/>\n").unwrap();
    std::fs::write(root.join("po/LINGUAS"), "de\n").unwrap();
    std::fs::write(root.join("x.pc.in"), "prefix=@prefix@\nName: x\n").unwrap();
    let r = decay_meson_eval::eval(&TestOracle, &TestSources, &root);
    std::fs::remove_dir_all(&root).ok();
    r.map(|(graph, _)| graph)
}

fn cmd(graph: &Graph, name: &str) -> Vec<CmdArg> {
    let target = graph
        .targets
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no target `{name}`"));
    assert!(matches!(target.kind, Kind::Custom));
    target.attrs.cmd.iter().map(|v| v.value.clone()).collect()
}

const ROOT: &str = "project('t', 'c')\ni18n = import('i18n')\nsubdir('data')\n";

#[test]
fn merge_file_runs_msgfmt_like_meson() {
    let graph = eval(
        "xml",
        ROOT,
        r#"
i18n.merge_file(
  input: 'mime.xml.in',
  output: 'mime.xml',
  data_dirs: '.',
  po_dir: '../po',
  type: 'xml',
)
"#,
    )
    .unwrap();
    let cmd = cmd(&graph, "mime.xml");
    let [
        CmdArg::Env(var, dirs),
        CmdArg::Target(_),
        CmdArg::Literal(kind),
        CmdArg::Literal(d),
        CmdArg::File(po),
        CmdArg::Literal(template),
        CmdArg::Inputs,
        CmdArg::Literal(o),
        CmdArg::Outputs,
    ] = cmd.as_slice()
    else {
        panic!("unexpected command {cmd:?}");
    };
    assert_eq!(var, "GETTEXTDATADIRS");
    assert_eq!(dirs, &[PathBuf::from("data")]);
    assert_eq!(kind, "--xml");
    assert_eq!(d, "-d");
    assert_eq!(po, Path::new("po"));
    assert_eq!(template, "--template");
    assert_eq!(o, "-o");
}

#[test]
fn merge_file_without_data_dirs_sets_no_environment() {
    let graph = eval(
        "desktop",
        ROOT,
        r#"
i18n.merge_file(
  input: 'mime.xml.in',
  output: 'mime.desktop',
  po_dir: '../po',
  type: 'desktop',
  args: ['--keyword=Name'],
)
"#,
    )
    .unwrap();
    let cmd = cmd(&graph, "mime.desktop");
    assert!(matches!(cmd[0], CmdArg::Target(_)), "{cmd:?}");
    assert_eq!(cmd[1], CmdArg::Literal("--desktop".to_owned()));
    assert_eq!(
        cmd.last(),
        Some(&CmdArg::Literal("--keyword=Name".to_owned()))
    );
}

#[test]
fn merge_file_rejects_a_missing_po_dir() {
    let err = eval(
        "nopo",
        ROOT,
        "i18n.merge_file(input: 'mime.xml.in', output: 'mime.xml', po_dir: '../nope')\n",
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains("po_dir"), "{err:?}");
}

#[test]
fn configure_file_output_names_its_input() {
    let graph = eval(
        "basename",
        r#"
project('t', 'c')
configure_file(input: 'x.pc.in', output: '@BASENAME@', configuration: {'prefix': '/usr'})
configure_file(input: 'x.pc.in', output: '@PLAINNAME@.copy', configuration: {'prefix': '/usr'})
"#,
        "",
    )
    .unwrap();
    let names: Vec<&str> = graph.targets.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"x.pc"), "{names:?}");
    assert!(names.contains(&"x.pc.in.copy"), "{names:?}");
    assert!(
        graph.provides.iter().any(|p| p.name == "x"),
        "the `.pc` is a provide"
    );
}
