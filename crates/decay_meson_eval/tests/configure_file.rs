//! `configure_file(format:)` refuses what its `sed` rewrite could not
//! reproduce, rather than emitting something that differs from meson.

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
        vec!["linux".to_owned()]
    }
}

/// The error evaluating `t.h.in` = `template` under `format` gives.
fn error(name: &str, format: &str, template: &str) -> String {
    let root = std::env::temp_dir().join(format!(
        "decay-configure-file-{name}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("meson.build"),
        format!(
            "project('t')\nconf = configuration_data()\nconf.set('A', 1)\n\
             configure_file(input: 't.h.in', output: 't.h', format: '{format}', \
             configuration: conf)\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join("t.h.in"), template).unwrap();
    let r = decay_meson_eval::eval(&TestOracle, &TestSources, &root);
    std::fs::remove_dir_all(&root).ok();
    match r {
        Ok(_) => panic!("`{template}` under `{format}` was not refused"),
        Err(err) => format!("{err:?}"),
    }
}

#[test]
fn an_unknown_format_is_refused() {
    assert!(error("unknown", "cmake2", "").contains("'cmake2'"));
}

#[test]
fn a_cmakedefine_in_a_meson_template_is_refused() {
    let err = error("meson", "meson", "#cmakedefine A\n");
    assert!(err.contains("format: 'cmake'"), "{err}");
}

#[test]
fn a_mesondefine_in_a_cmake_template_is_refused() {
    let err = error("mesondefine", "cmake", "#mesondefine A\n");
    assert!(err.contains("#mesondefine"), "{err}");
}

#[test]
fn an_indented_cmakedefine_is_refused() {
    let err = error("indented", "cmake", "  #cmakedefine A 1\n");
    assert!(err.contains("indented"), "{err}");
}

#[test]
fn a_bare_variable_token_is_refused() {
    let err = error("bare", "cmake", "#cmakedefine B A\n");
    assert!(err.contains("bare token"), "{err}");
}

#[test]
fn a_nested_variable_is_refused() {
    let err = error("nested", "cmake", "${A${A}}\n");
    assert!(err.contains("nested"), "{err}");
}
