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

    /// Every option name the user pinned, so the evaluator can reject a pin
    /// that names an option the project never declares (a typo, or the
    /// backend-sanitised name rather than the meson one).
    fn pinned_options(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether the importer was given a target for a program the build looks
    /// up on the machine it runs on.
    ///
    /// A build system's tools are not part of what it produces, and a build
    /// graph has no way to reach outside itself for one, so a program nobody
    /// supplied is absent rather than a question left open.
    fn has_program(&self, name: &str) -> bool {
        let _ = name;
        false
    }

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

    /// The answer to a toolchain probe, when it follows from something the
    /// importer already knows.
    ///
    /// `name` is the check (`has_function`) and `what` its argument
    /// (`dlvsym`). Answering here keeps the probe out of the generated build:
    /// either it is settled everywhere, or it is a question about the
    /// operating system, which the build already has a constraint for.
    fn probe(&self, name: &str, what: &str) -> Option<Probe> {
        let _ = (name, what);
        None
    }

    /// The answer to a probe the importer can settle by actually compiling
    /// it — once for every target in its configured matrix — rather than
    /// leaving it an open knob.
    ///
    /// Consulted only for the plain shapes [`CompileProbe`] can reproduce
    /// faithfully (no project `args:`/`dependencies:` the importer cannot
    /// replay). `None` leaves the probe open, exactly as [`Oracle::probe`]
    /// returning `None` does.
    fn compile_probe(&self, probe: &CompileProbe) -> Option<Probe> {
        let _ = probe;
        None
    }

    /// Whether toolchain and dependency probes (`cc.has_header`,
    /// `dependency()`, ...) should be left open.
    ///
    /// Leaving them open is the honest answer — the importer cannot run the
    /// compiler — but it does grow the configuration space.
    fn probes_are_open(&self) -> bool {
        true
    }

    /// The `pkg-config` variables of a dependency, e.g. `prefix` for
    /// `dependency('iso-codes').get_variable(pkgconfig: 'prefix')`.
    ///
    /// A dependency is still found or not as a build-time choice; this only
    /// supplies what its variables would read once it is. Left empty, a build
    /// that asks for one in a configuration where the dependency is found
    /// fails to import rather than silently making one up.
    fn dependency_variables(&self, name: &str) -> Vec<(String, String)> {
        let _ = name;
        Vec::new()
    }

    /// Whether a dependency is known to be found, when the importer already
    /// knows — e.g. it names another project the importer is building anyway,
    /// or the configuration supplied a target for it, not something that may
    /// or may not be on a machine.
    ///
    /// A `Probe::Fixed(true)` settles it found everywhere with no knob;
    /// [`Probe::Systems`] / [`Probe::Constraint`] settle it found only where a
    /// constraint holds and *false* (still no knob) everywhere else — a
    /// library that exists only on one OS, say. Left `None`, whether it is
    /// found stays a build-time choice, same as an environment probe.
    fn dependency_found(&self, name: &str) -> Option<Probe> {
        let _ = name;
        None
    }

    /// Whether `cc.find_library('name')` resolves, when that follows from the
    /// target system rather than from a knob a generated build should carry.
    ///
    /// A library the C runtime splits out (`m`, `dl`, `rt`, `pthread`,
    /// `resolv`, ...) or that the OS itself ships (`ws2_32`, `iphlpapi`, ...)
    /// is present as a fact about `os`/`abi`/`cpu`, not a `<lib>[true/false]`
    /// question. Answered like [`Oracle::probe`] and turned into a presence
    /// condition the same way. Left `None`, `find_library()` stays an open
    /// knob.
    fn system_library(&self, name: &str) -> Option<Probe> {
        let _ = name;
        None
    }

    /// The answer to a dependency's `1`-or-`0` `pkg-config` variable (an
    /// availability flag, the way `graphene_has_sse2` is), when it follows
    /// from something the importer already knows — answered the same way as
    /// [`Oracle::probe`], and for the same reason: whether SSE2 is available
    /// follows from the CPU, not from a knob a generated build should carry
    /// of its own.
    ///
    /// Left unanswered, a project reading the variable with no fallback of
    /// its own fails to import rather than a value being made up for it —
    /// same as [`Oracle::dependency_variables`], just for a flag instead of a
    /// fixed string.
    fn dependency_variable(&self, dep: &str, variable: &str) -> Option<Probe> {
        let _ = (dep, variable);
        None
    }

    /// The size or alignment of a C type, when the configuration pins it.
    ///
    /// `cc.sizeof()` and `cc.alignment()` yield a concrete integer that a
    /// generated header bakes in; there is no knob to leave open. Left
    /// unanswered, the executor refuses the call and asks for a `[sizeof]` /
    /// `[alignment]` entry rather than guessing.
    fn type_size(&self, query: SizeQuery, type_name: &str) -> Option<SizeAnswer> {
        let _ = (query, type_name);
        None
    }
}

/// What a toolchain probe answers, when the configuration knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The same answer in every configuration.
    Fixed(bool),
    /// True exactly on these systems, named as the importer's configuration
    /// names them.
    ///
    /// Distinct from [`Probe::Constraint`] because the system is already a
    /// variable of its own: an answer that depends on it has to ask that
    /// variable, or the two could disagree.
    Systems(Vec<String>),
    /// True exactly when the constraint `setting` holds one of `values`.
    Constraint {
        /// Label of the constraint setting, e.g. `prelude//abi/constraints:abi`.
        setting: String,
        /// Every value of the setting the configuration mentions. Whatever it
        /// does not mention is one further choice the importer cannot name.
        domain: Vec<String>,
        /// The values this probe holds for, a subset of `domain`.
        values: Vec<String>,
    },
    /// True on these systems when one of `rows` also holds — and, everywhere
    /// else, left open, exactly as if the oracle had answered nothing at all.
    ///
    /// For a fact a *partial* database knows (glibc's or musl's symbols, say)
    /// rather than an oracle that truly knows the whole answer:
    /// [`Probe::Constraint`] alone would overreach, since a constraint can be
    /// shared across operating systems the way buck2's `abi` is (`abi[gnu]`
    /// also names mingw on Windows, not just glibc on Linux) — asserting
    /// *false* wherever the system does not match would claim a symbol
    /// absent on a libc the database simply has no answer for.
    ///
    /// `rows` names each *combination* the database actually confirmed,
    /// rather than ranging every axis independently: a symbol musl exports on
    /// `arm64` but glibc does not, say, must not turn into `abi ∈ {gnu,
    /// musl}` ANDed with `cpu ∈ {arm64}` — that would round up to a
    /// rectangle and also claim `gnu`+`arm64`, a combination nothing
    /// confirmed. Only the exact rows are settled; the rest — including any
    /// combination of individually-mentioned values that is not itself a
    /// row — stays exactly as configurable as an unanswered probe.
    SystemsAndConstraint {
        systems: Vec<String>,
        /// One constraint setting and its known domain per axis (e.g.
        /// `abi`, `cpu`), shared across every row.
        axes: Vec<(String, Vec<String>)>,
        /// Each confirmed combination, one value per entry of `axes`, in the
        /// same order.
        rows: Vec<Vec<String>>,
    },
    /// Found on exactly the listed `(system, abi?)` rows; settled *false* on
    /// every other configured system, with no knob anywhere — the importer
    /// determined the answer for the whole configured matrix (a `zig cc
    /// -target` link probe per system, plus the libc database), so nothing is
    /// left open.
    ///
    /// For `cc.find_library()` of a runtime/OS library: `m` resolves on
    /// `linux`/`macos`/`freebsd` and on `windows` only under `abi[gnu]`
    /// (mingw), never `abi[msvc]`; `dl` not on `windows` at all. Each `found`
    /// entry is a system name (resolved like [`Probe::Systems`]) and the abi
    /// values it holds for — empty meaning every abi.
    PerSystem {
        /// The abi constraint setting and its known domain, for entries that
        /// name specific abis.
        abi: (String, Vec<String>),
        found: Vec<(String, Vec<String>)>,
    },
    /// True on exactly the `rows` that hold — and, on the named `systems`,
    /// *false* everywhere else, with no knob: the probe was compiled for
    /// every `(cpu, abi, ...)` combination those systems have, so within
    /// them the answer is complete. Only outside `systems` — a target the
    /// importer did not build the probe for — does it stay open, the same
    /// as an unanswered probe.
    ///
    /// Distinct from [`Probe::SystemsAndConstraint`], which leaves the
    /// complement open even inside its systems because its source (a
    /// partial symbol database) genuinely does not know it; here the
    /// compiler answered for the whole matrix.
    Matrix {
        systems: Vec<String>,
        /// One constraint setting and its known domain per axis, as in
        /// [`Probe::SystemsAndConstraint`].
        axes: Vec<(String, Vec<String>)>,
        /// Each combination the probe compiled for, one value per axis.
        rows: Vec<Vec<String>>,
    },
}

/// A compiler probe the importer can answer by building it, once per target
/// in its configured matrix. Carries everything needed to reconstruct the
/// translation unit meson would have compiled, plus the call's `args:` when
/// they are plain compiler flags the importer can replay (`Vec::new()`
/// otherwise — a probe carrying anything it cannot replay never reaches here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileProbe {
    pub kind: CompileProbeKind,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileProbeKind {
    /// `cc.has_header('h')` with no `prefix:`/`dependencies:`.
    Header { header: String },
    /// `cc.has_type('t', prefix: p)` with no `dependencies:`.
    Type { name: String, prefix: String },
    /// `cc.compiles(code, prefix: p)` with no `dependencies:`.
    Compiles { prefix: String, code: String },
}

impl CompileProbe {
    /// The C translation unit to compile (`zig cc -c`; presence is "does it
    /// compile", never "does it link").
    pub fn snippet(&self) -> String {
        match &self.kind {
            CompileProbeKind::Header { header } => format!("#include <{header}>\n"),
            CompileProbeKind::Type { name, prefix } => {
                format!("{prefix}\nvoid _decay_probe(void) {{ sizeof({name}); }}\n")
            }
            CompileProbeKind::Compiles { prefix, code } => format!("{prefix}\n{code}\n"),
        }
    }

    /// The call's `args:`, replayed on the `zig cc` line.
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// A value pinned by the importer's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pinned {
    Bool(bool),
    Int(i64),
    Str(Rc<str>),
    List(Vec<Rc<str>>),
    /// One scalar value per value of a constraint the build already selects
    /// on — keyed the same way as [`Probe::Constraint`] and
    /// [`SizeAnswer::Constraint`], so an option pinned this way shares the
    /// one constraint variable. `default` is the value everywhere `cases`
    /// does not name; without it those configurations are left uncovered.
    ByConstraint {
        setting: String,
        domain: Vec<String>,
        cases: Vec<(String, Box<Pinned>)>,
        default: Option<Box<Pinned>>,
    },
}

/// Which of `cc.sizeof()` / `cc.alignment()` is being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeQuery {
    Sizeof,
    Alignment,
}

impl SizeQuery {
    pub fn as_str(self) -> &'static str {
        match self {
            SizeQuery::Sizeof => "sizeof",
            SizeQuery::Alignment => "alignment",
        }
    }
}

/// What the configuration answers for `cc.sizeof()` / `cc.alignment()` on
/// one type. The value is a concrete integer a generated header bakes in, so
/// it cannot be left open — unanswered, the executor refuses the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SizeAnswer {
    /// The same number in every configuration.
    Fixed(i64),
    /// One number per value of a constraint the build already selects on —
    /// keyed the same way as [`Probe::Constraint`], so a `[sizeof]` answer
    /// and a `[probes]` answer on one setting share a single constraint.
    Constraint {
        /// Label of the constraint setting, e.g. `prelude//cpu/constraints:cpu`.
        setting: String,
        /// Every value of the setting the configuration mentions.
        domain: Vec<String>,
        /// The size for each value the answer names, a subset of `domain`.
        cases: Vec<(String, i64)>,
    },
}
