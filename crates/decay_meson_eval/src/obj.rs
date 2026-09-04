use {
    decay_meson_logic::{
        Pc,
        Variational, //
    },
    decay_build_ir::TargetId,
    eyre::bail,
    std::{
        cell::RefCell,
        hash::{
            Hash,
            Hasher, //
        },
        mem,
        rc::Rc,
        str::FromStr, //
    },
};

/// The non-primitive meson values: everything reached by calling a method on
/// something rather than by writing a literal.
#[derive(Debug, Clone)]
pub enum Obj {
    /// The `meson` global.
    Meson,
    /// `host_machine` / `build_machine` / `target_machine`.
    Machine(Machine),
    /// The object `meson.get_compiler()` returns.
    Compiler(Lang),
    /// A module brought in by `import()`.
    Module(Module),
    /// `configuration_data()`. Mutable, and shared by reference the way meson
    /// shares it.
    ConfigData(Rc<RefCell<ConfigData>>),
    /// Anything that can appear in `dependencies:`.
    Dep(Rc<Dep>),
    /// `find_program()`.
    Program(Rc<Program>),
    /// A build target: a library, an executable, a `custom_target`.
    Target(TargetId),
    /// A single named output of a multi-output `custom_target`.
    Output(TargetId, usize),
    /// `include_directories()`.
    IncludeDirs(Rc<Vec<String>>),
    /// A source file, as a path relative to the project root. `files()` binds
    /// paths at its call site, which is why they cannot stay plain strings.
    File(Rc<str>),
    /// `environment()`.
    Env,
    /// A `feature` option: `enabled`, `disabled` or `auto`.
    Feature(Rc<str>),
    /// `disabler()`: found nowhere, in place of whatever it stands in for.
    ///
    /// Real meson also has it disable any call it is passed to as an
    /// argument, anywhere; nothing here does that yet; a project that relies
    /// on it will have to say so with a clear error instead of silently
    /// behaving as if it were found.
    Disabler,
}

impl Obj {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Meson => "meson",
            Self::Machine(_) => "machine",
            Self::Compiler(_) => "compiler",
            Self::Module(_) => "module",
            Self::ConfigData(_) => "cfg_data",
            Self::Dep(_) => "dep",
            Self::Program(_) => "external_program",
            Self::Target(_) => "build_tgt",
            Self::Output(..) => "file",
            Self::IncludeDirs(_) => "inc",
            Self::File(_) => "file",
            Self::Env => "env",
            Self::Feature(_) => "feature",
            Self::Disabler => "disabler",
        }
    }
}

/// Objects are compared by identity where meson compares by identity, so a
/// `configuration_data()` handed around stays one object.
impl PartialEq for Obj {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Meson, Self::Meson) | (Self::Env, Self::Env) => true,
            (Self::Disabler, Self::Disabler) => true,
            (Self::Machine(a), Self::Machine(b)) => a == b,
            (Self::Compiler(a), Self::Compiler(b)) => a == b,
            (Self::Module(a), Self::Module(b)) => a == b,
            (Self::ConfigData(a), Self::ConfigData(b)) => Rc::ptr_eq(a, b),
            (Self::Dep(a), Self::Dep(b)) => Rc::ptr_eq(a, b),
            (Self::Program(a), Self::Program(b)) => Rc::ptr_eq(a, b),
            (Self::Target(a), Self::Target(b)) => a == b,
            (Self::Output(a, i), Self::Output(b, j)) => a == b && i == j,
            (Self::IncludeDirs(a), Self::IncludeDirs(b)) => a == b,
            (Self::File(a), Self::File(b)) => a == b,
            (Self::Feature(a), Self::Feature(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Obj {}

/// Consistent with [`PartialEq`]: the identity-compared variants
/// (`ConfigData`, `Dep`, `Program`) hash their `Rc` pointer, the rest hash
/// their contents.
impl Hash for Obj {
    fn hash<H: Hasher>(&self, state: &mut H) {
        mem::discriminant(self).hash(state);
        match self {
            Self::Meson | Self::Env | Self::Disabler => {}
            Self::Machine(m) => m.hash(state),
            Self::Compiler(l) => l.hash(state),
            Self::Module(m) => m.hash(state),
            Self::ConfigData(a) => Rc::as_ptr(a).hash(state),
            Self::Dep(a) => Rc::as_ptr(a).hash(state),
            Self::Program(a) => Rc::as_ptr(a).hash(state),
            Self::Target(t) => t.hash(state),
            Self::Output(t, i) => (t, i).hash(state),
            Self::IncludeDirs(d) => d.hash(state),
            Self::File(f) | Self::Feature(f) => f.hash(state),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Machine {
    Host,
    Build,
    Target,
}

impl Machine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Build => "build",
            Self::Target => "target",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    C,
    Cpp,
    ObjC,
    ObjCpp,
    Rust,
    Fortran,
    D,
}

impl Lang {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::ObjC => "objc",
            Self::ObjCpp => "objcpp",
            Self::Rust => "rust",
            Self::Fortran => "fortran",
            Self::D => "d",
        }
    }
}

impl FromStr for Lang {
    type Err = eyre::Report;

    fn from_str(s: &str) -> eyre::Result<Self> {
        Ok(match s {
            "c" => Self::C,
            "cpp" | "c++" => Self::Cpp,
            "objc" => Self::ObjC,
            "objcpp" => Self::ObjCpp,
            "rust" => Self::Rust,
            "fortran" => Self::Fortran,
            "d" => Self::D,
            _ => bail!("unknown language `{s}`"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Module {
    PkgConfig,
    Python,
    GNOME,
    I18n,
    Fs,
    Windows,
    Other,
}

impl FromStr for Module {
    type Err = eyre::Report;

    fn from_str(s: &str) -> eyre::Result<Self> {
        Ok(match s {
            "pkgconfig" => Self::PkgConfig,
            "python" | "python3" => Self::Python,
            "gnome" => Self::GNOME,
            "i18n" => Self::I18n,
            "fs" => Self::Fs,
            "windows" => Self::Windows,
            _ => Self::Other,
        })
    }
}

/// Accumulated `conf.set(...)` calls, each remembering the configurations it
/// was made under.
#[derive(Debug, Default, Clone)]
pub struct ConfigData {
    pub entries: Vec<(Rc<str>, Variational<Entry>)>,
}

impl ConfigData {
    pub fn get(&self, name: &str) -> Option<&Variational<Entry>> {
        self.entries.iter().find(|(k, _)| &**k == name).map(|(_, v)| v)
    }

    pub fn slot(&mut self, name: &Rc<str>) -> &mut Variational<Entry> {
        if let Some(i) = self.entries.iter().position(|(k, _)| k == name) {
            return &mut self.entries[i].1;
        }
        self.entries.push((name.clone(), Variational::empty()));
        &mut self.entries.last_mut().unwrap().1
    }
}

/// How a configuration entry should be written into the generated header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Entry {
    /// `set_quoted`.
    Quoted(Rc<str>),
    /// `set` with a string.
    Raw(Rc<str>),
    Int(i64),
    /// `set` with a bool: defined when true, undefined when false.
    Flag(bool),
    /// `set10`: always defined, as `1` or `0`.
    Ten(bool),
}

/// Anything usable in `dependencies:`, whether it comes from outside the build
/// or from `declare_dependency()`.
#[derive(Debug)]
pub struct Dep {
    pub name: String,
    /// The configurations in which the dependency was actually found.
    pub found: Pc,
    /// The interface target carrying its usage requirements.
    pub target: TargetId,
    /// `pkgconfig`, `library`, `internal`, ...
    pub type_name: &'static str,
    pub version: Option<String>,
    pub variables: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct Program {
    pub name: String,
    pub found: Pc,
    pub target: TargetId,
    /// Set when the program is a file inside the source tree.
    pub path: Option<String>,
}
