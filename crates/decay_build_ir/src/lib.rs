//! A build graph that has forgotten it ever was meson.
//!
//! The executor produces one of these; a backend turns it into build files.
//! Everything a configuration can influence is a [`Variational`] list, so a
//! backend never has to ask "which configuration is this?" — it just has to know
//! how to render a presence condition in its own dialect (a `select()`, a
//! conditional block, a set of variants).

use {
    decay_meson_logic::{
        Pc,
        Var,
        Variational, //
    },
    std::{
        fmt::{
            self,
            Display, //
        },
        path::PathBuf,
    },
};

pub mod graph;

pub use graph::Graph;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TargetId(pub u32);

impl Display for TargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

/// What the project as a whole is.
#[derive(Debug, Default, Clone)]
pub struct Project {
    pub name: String,
    pub version: Option<String>,
    pub license: Vec<String>,
    pub languages: Vec<String>,
    /// Where the sources come from.
    ///
    /// The graph names files by their path inside the project, and says nothing
    /// about where that project lives; a backend that fetches sources rather
    /// than keeping a copy of them needs to be told.
    pub origin: Option<Origin>,
}

/// An upstream revision to fetch the sources from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub repo: String,
    /// A full commit hash: anything else would let the imported sources move
    /// under a build that is supposed to be reproducible.
    pub rev: String,
}

/// A node in the build graph.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: TargetId,
    /// Unique, backend-safe name.
    pub name: String,
    /// The name the meson sources used, kept for diagnostics.
    pub label: String,
    /// Directory the declaring `meson.build` lived in, relative to the project
    /// root. Backends that have a notion of packages use it as one.
    pub package: PathBuf,
    /// The configurations in which this target exists at all.
    pub cond: Pc,
    pub kind: Kind,
    pub attrs: Attrs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `static_library()`.
    StaticLibrary,
    /// `shared_library()` / `shared_module()`.
    SharedLibrary,
    /// `library()`: linkage is whatever `default_library` resolves to, which is
    /// itself usually a configuration variable.
    Library { linkage: Variational<Linkage> },
    Executable,
    /// `custom_target()`: run a command to produce files.
    Custom,
    /// `configure_file()` with a `configuration:` — a generated header.
    ConfigHeader,
    /// `declare_dependency()`: no build action, only usage requirements.
    Interface,
    /// Something resolved outside the build.
    External(External),
}

impl Kind {
    pub fn is_external(&self) -> bool {
        matches!(self, Kind::External(_))
    }

    /// Whether the target produces linkable output that dependents consume.
    pub fn is_linkable(&self) -> bool {
        matches!(
            self,
            Kind::StaticLibrary
                | Kind::SharedLibrary
                | Kind::Library { .. }
                | Kind::Interface
                | Kind::External(_)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Linkage {
    Static,
    Shared,
    Both,
}

/// How a dependency that the build does not itself produce is found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum External {
    /// `dependency('gl')`.
    PkgConfig { module: String },
    /// `cc.find_library('dl')`.
    SystemLibrary { name: String },
    /// `dependency('appleframeworks', modules: [...])`.
    Framework { modules: Vec<String> },
    /// `find_program('doxygen')`.
    Program { name: String, path: Option<PathBuf> },
}

/// Everything about a target that a configuration can change.
#[derive(Debug, Default, Clone)]
pub struct Attrs {
    pub srcs: Variational<Source>,
    /// Headers that belong to the target and are visible to its dependents.
    pub headers: Variational<Source>,
    /// Include directories, relative to the project root.
    pub include_dirs: Variational<PathBuf>,
    pub compile_args: Variational<String>,
    pub link_args: Variational<String>,
    pub deps: Variational<TargetId>,
    /// Targets linked into this one without inheriting their usage
    /// requirements (`link_with:`).
    pub link_with: Variational<TargetId>,
    /// Command line for [`Kind::Custom`] targets.
    pub cmd: Variational<CmdArg>,
    /// Files a [`Kind::Custom`] or [`Kind::ConfigHeader`] target produces.
    pub outs: Vec<String>,
    /// `#define`s for a [`Kind::ConfigHeader`].
    pub defines: Variational<Define>,
    /// The `.in` file a [`Kind::ConfigHeader`] substitutes into, when it has
    /// one instead of being generated from scratch.
    pub template: Option<Source>,
    /// Set on a shared library that carries an soname/compatibility version.
    pub version: Option<String>,
    /// The configurations in which the target's output gets installed.
    pub install: Pc,
    /// Where installed output lands, when it is not the default for the kind.
    pub install_dir: Option<String>,
    /// Free-form key/value pairs a consumer may read back.
    pub variables: Variational<(String, String)>,
}

/// An input file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// A path relative to the project root.
    File(PathBuf),
    /// Every output of another target.
    Generated(TargetId),
}

/// One word of a [`Kind::Custom`] command line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CmdArg {
    Literal(String),
    /// The program to run, as a target that resolves to an executable.
    Target(TargetId),
    /// A file, spelled the way the backend spells file references.
    File(PathBuf),
    /// Meson's `@INPUT@`, `@OUTPUT@`, `@OUTDIR@`.
    Inputs,
    Outputs,
    OutDir,
}

/// One entry of a generated configuration header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Define {
    pub name: String,
    pub value: DefineValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DefineValue {
    /// `#define NAME "value"`.
    Quoted(String),
    /// `#define NAME value`.
    Raw(String),
    /// `#define NAME 1` / `0`.
    Number(i64),
    /// `#define NAME` when set, `/* #undef NAME */` when not.
    Flag,
    /// Present in no configuration: emitted as `#undef`.
    Undef,
}

/// A test the project declares.
#[derive(Debug, Clone)]
pub struct Test {
    pub name: String,
    pub target: TargetId,
    pub cond: Pc,
    pub args: Variational<String>,
}

/// Files installed on their own rather than as a target's output, e.g. public
/// headers declared with `install_headers()`.
#[derive(Debug, Clone)]
pub struct Install {
    pub files: Variational<Source>,
    /// Sub-directory under the install root, when one was given.
    pub subdir: Option<String>,
    pub cond: Pc,
}

/// A configuration variable the executor had to leave open, mirrored out of the
/// logic arena so backends need not depend on it.
pub type Option_ = Var;
