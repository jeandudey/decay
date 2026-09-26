//! A `configure_file()` template comes out of the emitted `sed` exactly as
//! meson's own `meson setup` writes it.

use {
    decay_buck2::{
        Labels,
        Shared, //
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
    std::{
        path::{
            Path,
            PathBuf, //
        },
        process::Command,
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

/// Every value kind `configuration_data()` holds, including the ones a
/// `sed` replacement or C string has to escape.
const CONF: &str = r#"
conf = configuration_data()
conf.set('FLAG', true)
conf.set('FALSE_V', false)
conf.set('ZERO', 0)
conf.set('NUM', 42)
conf.set('EMPTY', '')
conf.set('STR', 'hello  world')
conf.set_quoted('QUOTED', 'say "hi" \\ there')
conf.set10('TEN', true)
conf.set10('TEN0', false)
conf.set('SPECIAL', 'a&b|c\\d')
"#;

/// Runs the `sed` the emitted genrule for `output` would, on `project`'s own
/// template.
fn emitted(project: &Path, output: &str, template: &str) -> String {
    let (graph, mut logic) =
        decay_meson_eval::eval(&TestOracle, &TestSources, project).expect("evaluates");
    let shared = Shared::collect("shared".to_owned(), [&graph]);
    let out = project.join("buck");
    let build = decay_buck2::emit(&graph, &mut logic, &Labels::default(), &shared, &out, "pkg")
        .expect("emits");

    let rule = build
        .split_once(&format!("name = \"{output}\","))
        .unwrap_or_else(|| panic!("no rule for `{output}`:\n{build}"))
        .1;
    let cmd = rule
        .split_once("cmd = ")
        .expect("a genrule")
        .1
        .split_once(",\n")
        .expect("one line")
        .0;
    let mut script = starlark_concat(cmd);
    let location = script.find("$(location ").expect("names its template");
    let end = location + script[location..].find(')').unwrap() + 1;
    script.replace_range(location..end, &project.join(template).display().to_string());

    let target = project.join(format!("{output}.decay"));
    let status = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env("OUT", &target)
        .status()
        .expect("runs sh");
    assert!(status.success(), "{script}");
    std::fs::read_to_string(target).unwrap()
}

/// The string a `"a" + "b"` Starlark expression evaluates to.
fn starlark_concat(expr: &str) -> String {
    let mut out = String::new();
    let mut chars = expr.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '+' => {}
            '"' => loop {
                match chars.next().expect("closed string") {
                    '"' => break,
                    '\\' => match chars.next().unwrap() {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        other => out.push(other),
                    },
                    other => out.push(other),
                }
            },
            _ => panic!("not a plain string concatenation: {expr}"),
        }
    }
    out
}

/// What `meson setup` itself writes for `output`.
fn meson(project: &Path, output: &str) -> String {
    let build = project.join("meson-build");
    let status = Command::new("python3")
        .args(["-m", "mesonbuild.mesonmain", "setup", "--backend=none"])
        .arg(&build)
        .arg(project)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("runs meson");
    assert!(status.success());
    std::fs::read_to_string(build.join(output)).unwrap()
}

fn project(name: &str, build: &str, template: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("decay-configure-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("meson.build"), build).unwrap();
    std::fs::write(root.join("t.h.in"), template).unwrap();
    root
}

const CMAKE_TEMPLATE: &str = "/* mail me@example.com, or @ alone */
#cmakedefine FLAG
#cmakedefine FLAG 1
#  cmakedefine   STR   @STR@    trailing\t tab
#cmakedefine FALSE_V 1
#cmakedefine MISSING 1
#cmakedefine ZERO 1
#cmakedefine NUM @NUM@
#cmakedefine EMPTY 1
#cmakedefine QUOTED
#cmakedefine TEN yes
#cmakedefine TEN0 yes
#cmakedefine NUM @MISSING@
#cmakedefine01 FLAG
#cmakedefine01 FALSE_V
#cmakedefine01 MISSING
#cmakedefine01 ZERO
#cmakedefine01 STR
#cmakedefine01 EMPTY
#cmakedefine01 TEN0 ignored tokens
#define A @STR@ @NUM@ @FALSE_V@ @FLAG@ @MISSING@ @QUOTED@ @TEN@ @TEN0@ @EMPTY@
#define B ${STR} ${FALSE_V} ${MISSING} @SPECIAL@ \"${SPECIAL}\"
#define C @not a name@ @dotted.name-1@
";

fn check(format: &str) {
    let output = "out.h";
    let build = format!(
        "project('t')\n{CONF}\nconfigure_file(input: 't.h.in', output: '{output}', \
         format: '{format}', configuration: conf)\n"
    );
    let root = project(&format.replace('@', "at"), &build, CMAKE_TEMPLATE);
    let want = meson(&root, output);
    let got = emitted(&root, output, "t.h.in");
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(got, want);
}

#[test]
fn a_cmake_template_substitutes_like_meson() {
    check("cmake");
}

#[test]
fn a_cmake_at_template_substitutes_like_meson() {
    check("cmake@");
}

#[test]
fn a_meson_template_substitutes_quoted_and_special_values_like_meson() {
    let output = "out.h";
    let build = format!(
        "project('t')\n{CONF}\nconfigure_file(input: 't.h.in', output: '{output}', \
         configuration: conf)\n"
    );
    let root = project(
        "meson",
        &build,
        "#mesondefine QUOTED\n#mesondefine SPECIAL\n#define A @QUOTED@ @SPECIAL@ @NUM@\n",
    );
    let want = meson(&root, output);
    let got = emitted(&root, output, "t.h.in");
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(got, want);
}
