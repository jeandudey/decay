//! A build graph that has forgotten it ever was meson.
//!
//! The executor produces one of these; a backend turns it into build files.
//! Everything a configuration can influence is a [`Variational`] list, so a
//! backend never has to ask "which configuration is this?" — it just has to know
//! how to render a presence condition in its own dialect (a `select()`, a
//! conditional block, a set of variants).

use {
    decay_meson_logic::{
        Formula,
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
    /// A wrap's `patch_directory` overlay this project's sources were merged
    /// with before evaluation, when it named one and it actually came from
    /// wrapdb (not a `decay.toml`-local test fixture). The overlay itself
    /// never lands in `origin` — see [`Origin::Archive`] — so a referenced
    /// file it provided needs fetching from here instead.
    pub wrapdb_overlay: Option<WrapdbOverlay>,
    /// Every top-level variable this project's root `meson.build` (and
    /// anything it `subdir()`s into, which shares that same scope) bound to a
    /// single fixed string/bool/int across the whole configuration — what
    /// `subproject(this).get_variable(key)` reads from a consumer.
    /// Configuration-dependent variables are dropped, the same tradeoff
    /// `Package::variables` already makes for `pkg.generate()`.
    pub variables: Vec<(String, String)>,
}

impl Project {
    /// The name of the target that fetches this project's sources.
    ///
    /// Fixed rather than derived from the project name: every project gets
    /// its own package, so one name never clashes across projects, and a
    /// `library()` named after its own project (fribidi's) no longer lands
    /// on the fetch's label.
    pub fn repo_target(&self) -> String {
        "source".to_owned()
    }

    /// The name of the target that fetches wrapdb itself, for a file
    /// [`WrapdbOverlay`]'s `patch_directory` provided.
    pub fn wrapdb_target(&self) -> String {
        "wrapdb".to_owned()
    }
}

/// Where a project's sources are fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A git repository, pinned to a commit.
    Git {
        repo: String,
        /// A full commit hash: anything else would let the imported sources
        /// move under a build that is supposed to be reproducible.
        rev: String,
    },
    /// A tarball, as a meson wrap's `[wrap-file]` names one — still just the
    /// plain upstream tarball's own URL and hash even when the wrap also
    /// names a `patch_directory`: that overlay only ever applies to
    /// `decay`'s own working copy before it evaluates the project, never to
    /// what gets fetched here. A referenced file the overlay provided is
    /// addressed against [`Project::wrapdb_overlay`] instead — see
    /// `decay_buck2`'s `source_address`.
    Archive(ArchiveFile),
}

/// One archive to fetch and extract, content-addressed so the build stays
/// reproducible without pinning a revision the way git does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveFile {
    pub url: String,
    pub sha256: String,
    /// The single top-level directory the archive extracts into, if it has
    /// one, so a backend can strip it and land the project at the archive
    /// root the way `decay` evaluated it.
    pub strip_prefix: Option<String>,
}

/// A wrap's `patch_directory` overlay — wrapdb's own files for it, copied
/// onto the fetched source before `decay` ever evaluates the project (see
/// `WrapCache::materialize`/`materialize_git` in `src/wrap_cache.rs`). Not
/// part of `origin`'s own fetch, so a target that references one of
/// `paths` needs a second fetch, of wrapdb itself, pinned at `rev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapdbOverlay {
    /// The wrapdb commit `patch_directory`'s content was read from —
    /// `WrapFile::wrapdb_rev` in `src/wrapdb.rs`, the same commit
    /// `decay.lock` pins so a later run overlays the exact same files.
    pub rev: String,
    /// `subprojects/packagefiles/<patch_directory>` in wrapdb's own tree —
    /// the directory `paths` are relative to, once wrapdb itself is
    /// checked out.
    pub patch_directory: String,
    /// Every path, relative to the project root, this overlay provided.
    pub paths: std::collections::BTreeSet<PathBuf>,
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
    Library {
        linkage: Variational<Linkage>,
    },
    Executable,
    /// `custom_target()`: run a command to produce files.
    Custom,
    /// `configure_file()` with a `configuration:` — a generated header.
    ConfigHeader,
    /// `cc.preprocess()`: run the real C preprocessor over one source.
    Preprocess,
    /// `declare_dependency()`: no build action, only usage requirements.
    Interface,
    /// A `.rc` resource script. buck2 will not compile a `.rc` inside a
    /// `cxx_library`; it needs its own `windows_resource` rule, consumed as a
    /// `deps` entry of the target that links the resource.
    WindowsResource,
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
    /// `dependency('threads')` — meson's builtin threading dependency, always
    /// found: `-pthread` on every toolchain except MSVC, where threads are in
    /// the CRT and the flag is not understood.
    Threads,
    /// `dependency('iconv')` — meson's builtin iconv dependency, always found:
    /// the iconv API is in libc on glibc/musl and the BSDs, a standalone
    /// `-liconv` on macOS / Windows.
    Iconv,
    /// `dependency('intl')` — meson's builtin gettext dependency, always found:
    /// the gettext runtime is in libc on glibc/musl, a standalone `-lintl`
    /// everywhere else (the BSDs, macOS, Windows).
    Intl,
    /// `find_program('doxygen')`.
    Program { name: String, path: Option<PathBuf> },
}

/// Everything about a target that a configuration can change.
#[derive(Debug, Default, Clone)]
pub struct Attrs {
    pub srcs: Variational<Source>,
    /// Headers that belong to the target and are visible to its dependents.
    pub headers: Variational<Source>,
    /// Headers a compiled source `#include "…"`s from its own directory,
    /// which meson resolves through the implicit source-dir include path.
    /// Private to the target and keyed by the exact spelling used.
    pub sibling_headers: Variational<Source>,
    /// Include directories, relative to the project root.
    pub include_dirs: Variational<PathBuf>,
    /// Set when a checked-in header of this target has a quoted `#include`
    /// with `..`. The flat header symlink tree decay stages cannot reproduce
    /// the on-disk layout such an include walks out of, so the target is
    /// compiled against real `-I` roots into the fetched source tree instead.
    pub raw_include_roots: bool,
    /// Each `raw_include_roots`-triggering `#include "../x"` found: the
    /// including file's own directory, and the literal include string.
    /// Resolved at render time against the graph's generated targets, once
    /// every target exists to search — a `..` walk can land on a
    /// `custom_target()` output with no on-disk file at all (fontconfig's
    /// `fcstr.c` reaching `fc-case/fccase.h`), which no real `-I` root into
    /// the fetched tree can ever satisfy the same way a checked-in sibling
    /// can.
    pub dotdot_includes: Vec<(PathBuf, String)>,
    pub compile_args: Variational<Flag>,
    pub link_args: Variational<Flag>,
    pub deps: Variational<TargetId>,
    /// Targets linked into this one without inheriting their usage
    /// requirements (`link_with:`).
    pub link_with: Variational<TargetId>,
    /// Set when some target names this one in meson's `link_whole:` — every
    /// object of this library is pulled into whatever links it, even with no
    /// undefined reference. Lets a sourceless re-export `shared_library`
    /// (`link_whole: libfoo` and nothing else) produce a real `.so`.
    pub link_whole: bool,
    /// Command line for [`Kind::Custom`] targets.
    pub cmd: Variational<CmdArg>,
    /// Files a [`Kind::Custom`] or [`Kind::ConfigHeader`] target produces.
    pub outs: Vec<String>,
    /// `custom_target(capture: true)`: the command's stdout is the output,
    /// rather than the command writing it itself.
    pub capture: bool,
    /// `#define`s for a [`Kind::ConfigHeader`].
    pub defines: Variational<Define>,
    /// The `.in` file a [`Kind::ConfigHeader`] substitutes into, when it has
    /// one instead of being generated from scratch.
    pub template: Option<Source>,
    /// Which of meson's template syntaxes `template` is written in.
    pub template_format: TemplateFormat,
    /// Set on a shared library that carries an soname/compatibility version.
    pub version: Option<String>,
    /// The configurations in which the target's output gets installed.
    pub install: Pc,
    /// Where installed output lands, when it is not the default for the kind.
    pub install_dir: Option<String>,
    /// Free-form key/value pairs a consumer may read back.
    pub variables: Variational<(String, String)>,
}

/// `configure_file(format:)`: the syntax a template substitutes in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum TemplateFormat {
    /// `@NAME@` and `#mesondefine NAME`.
    #[default]
    Meson,
    /// `cmake`/`cmake@`: `@NAME@`, `${NAME}` (`cmake` only),
    /// `#cmakedefine NAME ...` and `#cmakedefine01 NAME`, with the names the
    /// template actually uses in each.
    Cmake(CmakeTemplate),
}

/// The names a `format: 'cmake'`/`'cmake@'` template substitutes, by syntax.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CmakeTemplate {
    /// `@NAME@`.
    pub at: std::collections::BTreeSet<String>,
    /// `${NAME}`; always empty for `cmake@`.
    pub brace: std::collections::BTreeSet<String>,
    /// `#cmakedefine NAME ...`.
    pub define: std::collections::BTreeSet<String>,
    /// `#cmakedefine01 NAME`.
    pub define01: std::collections::BTreeSet<String>,
}

/// An input file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// A path relative to the project root.
    File(PathBuf),
    /// One output of another target, by its index into that target's own
    /// `outs` — `0` for a target with exactly one output (the overwhelming
    /// common case), matching meson's own `custom_target()[i]` indexing. A
    /// multi-output target added to `sources:`/etc. without indexing (meson
    /// then means every one of its outputs) lowers to one `Generated` per
    /// index rather than losing all but the first.
    Generated(TargetId, usize),
}

/// One word of a [`Kind::Custom`] command line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CmdArg {
    Literal(String),
    /// The program to run, as a target that resolves to an executable.
    Target(TargetId),
    /// A file, spelled the way the backend spells file references.
    File(PathBuf),
    /// A literal prefix glued directly onto a file reference, no separating
    /// space (`--sourcedir=`/a source-tree path, from `'--sourcedir=' +
    /// meson.current_source_dir()`).
    PrefixedFile(String, PathBuf),
    /// An environment assignment for the command that follows, set to a
    /// `:`-separated list of project paths (`GETTEXTDATADIRS=<dir>:<dir>`,
    /// what meson's own `msgfmthelper` exports for `i18n.merge_file()`).
    Env(String, Vec<PathBuf>),
    /// Meson's `@INPUT@`, `@OUTPUT@`, `@OUTDIR@`.
    Inputs,
    Outputs,
    OutDir,
}

/// One `compile_args:`/`link_args:` entry: a plain flag, or one whose text
/// embeds a reference to a file this project's own graph provides.
///
/// `meson.current_build_dir()`/`current_source_dir()` have no real directory
/// to answer with at import time, so a flag built by `.format()`-interpolating
/// one — pcre2's `'-Wl,--version-script,@0@/lib@1@.sym'.format(meson.
/// current_build_dir(), lib)`, zlib's own version-script naming a plain file
/// in its checkout the same way — comes out as a path that only ever
/// resolved because decay evaluated it from that same checkout, unchanged.
/// `File` keeps the reference live instead: `prefix` is the flag's literal
/// text up to the embedded path, and `Source` is what actually answers it —
/// another target's declared output, or a file already in the project.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Flag {
    Literal(String),
    File(String, Source),
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
    /// Set to `false`: `/* #undef NAME */` like [`DefineValue::Undef`], but
    /// a cmake-format template substitutes it as `0` where an unset name
    /// substitutes nothing.
    False,
    /// Present in no configuration: emitted as `#undef`.
    Undef,
}

/// A test the project declares.
#[derive(Debug, Clone)]
pub struct Test {
    pub name: String,
    pub target: TargetId,
    pub cond: Pc,
    /// A literal, a file, or a reference to another target in the same
    /// project (a build target `args:` names directly, e.g. to hand a test
    /// its own library's path) — same shape as a `custom_target()` command.
    pub args: Variational<CmdArg>,
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

/// A `pkg-config` module this project makes available to whatever else
/// imports it, from `import('pkgconfig').generate()` or a `configure_file()`
/// that produces a `.pc` file directly.
///
/// Recording this is what lets one imported project's `dependency('name')`
/// resolve against another imported project instead of needing a hand-written
/// answer in `decay.toml` for something the importer already knows in full.
#[derive(Debug, Clone)]
pub struct Package {
    /// The name `dependency()` looks up, i.e. the `.pc` file's base name.
    pub name: String,
    /// The target carrying its usage requirements, when it is a linkable
    /// library and not just data.
    pub target: Option<TargetId>,
    /// The `.pc` file's `Requires:` — other package names a consumer of this
    /// one also needs on its include/link path, the way `pkg-config --cflags`
    /// pulls a `Requires:` in transitively.
    pub requires: Vec<String>,
    /// `pkg-config` variables resolved to their actual value, wherever the
    /// value does not itself depend on the configuration.
    pub variables: Vec<(String, String)>,
    /// The configurations the project provides it in, in the project's own
    /// arena.
    pub cond: Pc,
    /// [`Self::cond`] lifted out of that arena, for a sibling project's
    /// `dependency()` to read back. Filled in by the importer once evaluation
    /// is done; [`Formula::TRUE`] until then.
    pub found: Formula,
}

/// A configuration variable the executor had to leave open, mirrored out of the
/// logic arena so backends need not depend on it.
pub type Option_ = Var;
