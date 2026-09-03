use {
    crate::obj::Machine,
    std::rc::Rc, //
};

/// What the importer already knows, and does not have to leave open.
///
/// Anything the oracle declines to answer becomes a configuration variable: the
/// executor explores every value it could take and the backend turns that into
/// a build-time choice. Most projects should pin very little here — the whole
/// point is that the generated build stays as configurable as the meson one.
pub trait Oracle {
    /// A value the user pinned for a build option.
    fn option(&self, name: &str) -> Option<Pinned>;

    /// A machine property the user pinned, e.g. `system` or `cpu_family`.
    fn machine(&self, machine: Machine, property: &str) -> Option<String>;

    /// The systems a build may target, used as the domain of
    /// `host_machine.system()` when it is left open.
    fn systems(&self) -> Vec<String>;

    /// The compiler identities a build may use, used as the domain of
    /// `compiler.get_id()` when it is left open.
    fn compilers(&self) -> Vec<String> {
        ["gcc", "clang", "msvc"].map(str::to_owned).to_vec()
    }

    /// Whether toolchain and dependency probes (`cc.has_header`,
    /// `dependency()`, ...) should be left open.
    ///
    /// Leaving them open is the honest answer — the importer cannot run the
    /// compiler — but it does grow the configuration space.
    fn probes_are_open(&self) -> bool {
        true
    }
}

/// A value pinned by the importer's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pinned {
    Bool(bool),
    Int(i64),
    Str(Rc<str>),
    List(Vec<Rc<str>>),
}
