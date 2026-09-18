//! A computed dict key (`'cxx-@0@'.format(std): {...}`, the shape glib's test
//! suite uses) should evaluate rather than fail to parse or lower.

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

#[test]
fn a_dict_key_built_from_format_evaluates_per_loop_iteration() {
    let root = std::env::temp_dir().join(format!(
        "decay-computed-dict-key-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("meson.build"),
        r#"
project('t', 'c')
std_list = ['11', '14']
mytests = {}
foreach std : std_list
  mytests += {
    'cxx-@0@'.format(std) : ['main.c'],
  }
endforeach
foreach name, srcs : mytests
  executable(name, srcs)
endforeach
"#,
    )
    .unwrap();
    std::fs::write(root.join("main.c"), "int main(void) { return 0; }\n").unwrap();

    let oracle = TestOracle;
    let sources = TestSources;
    let (graph, _logic) = decay_meson_eval::eval(&oracle, &sources, &root).unwrap();

    let mut names: Vec<&str> = graph.targets.iter().map(|t| t.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["cxx-11", "cxx-14"]);

    std::fs::remove_dir_all(&root).ok();
}

/// The same (key, value) pair added under many separate, mutually exclusive
/// branch conditions -- the shape glib's test suite hits hundreds of times
/// while building up its `glib_tests` dict -- must fuse into one dict entry
/// (condition: the branches' disjunction) rather than growing the entry
/// count by one per branch. Without the `Variational::normalize` fusion in
/// `Interp::add`'s dict case, this used to grow unboundedly on every
/// re-scanned `+=` and blow past several GB before finishing.
#[test]
fn a_dict_merge_fuses_identical_entries_across_many_branches() {
    let root = std::env::temp_dir().join(format!(
        "decay-dict-merge-dedup-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let systems = TestOracle.systems();
    let mut source = String::from("project('t', 'c')\nd = {}\n");
    for system in &systems {
        source.push_str(&format!(
            "if host_machine.system() == '{system}'\n  d += {{'k': 'v'}}\nendif\n"
        ));
    }
    source.push_str("foreach name, val : d\n  executable(name, ['main.c'])\nendforeach\n");
    std::fs::write(root.join("meson.build"), source).unwrap();
    std::fs::write(root.join("main.c"), "int main(void) { return 0; }\n").unwrap();

    let oracle = TestOracle;
    let sources = TestSources;
    let (graph, _logic) = decay_meson_eval::eval(&oracle, &sources, &root).unwrap();

    assert_eq!(graph.targets.len(), 1);
    assert_eq!(graph.targets[0].name, "k");

    std::fs::remove_dir_all(&root).ok();
}
