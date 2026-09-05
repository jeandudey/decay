use {
    crate::{
        config::{
            Config,
            Machine,
            OptionValue,
            ProbeValue,
            Project,
            SizeValue, //
        },
        packages::Packages,
    },
    crate::probe::{
        self,
        ProbeCache, //
    },
    decay_meson_eval::{
        obj,
        oracle::{
            CompileProbe,
            Oracle,
            Pinned,
            Probe,
            SizeAnswer,
            SizeQuery, //
        },
    },
    std::{
        cell::RefCell,
        rc::Rc, //
    },
};

/// Answers from the importer's own configuration, and from whatever earlier
/// projects have already determined about themselves.
///
/// Everything it declines to answer is left open, which is the default: a
/// project's options should stay options in the generated build.
pub struct ConfigOracle<'a> {
    project: &'a Project,
    config: &'a Config,
    packages: &'a Packages,
    /// Memoised `zig cc` probe results — see [`Oracle::compile_probe`].
    probe_cache: RefCell<ProbeCache>,
}

impl<'a> ConfigOracle<'a> {
    pub fn new(config: &'a Config, project: &'a Project, packages: &'a Packages) -> Self {
        Self {
            project,
            config,
            packages,
            probe_cache: RefCell::default(),
        }
    }

    fn machine_config(&self, machine: obj::Machine) -> &'a Machine {
        match machine {
            obj::Machine::Build => &self.project.build_machine,
            // Meson treats the target machine as the host one unless the
            // project is cross-compiling a compiler, which this importer does
            // not model.
            obj::Machine::Host | obj::Machine::Target => &self.project.host_machine,
        }
    }

    /// The `[probes]` entry at `key`, turned into an [`Probe`] — shared by
    /// [`Oracle::probe`] (keyed `check:argument`) and
    /// [`Oracle::dependency_variable`] (keyed `dependency:name:variable`):
    /// both are "answer this from a constraint instead of leaving it open."
    fn probe_answer(&self, key: &str) -> Option<Probe> {
        let answer = self.config.probes.get(key)?;
        if let ProbeValue::Fixed(settled) = answer {
            return Some(Probe::Fixed(*settled));
        }

        // Checked when the configuration was loaded.
        let setting = answer.setting().ok().flatten()?;
        let values = answer.values();

        // An answer that names the constraint the system is selected on has to
        // ask the system variable itself. Two variables on one constraint could
        // disagree, and the generated `select()`s would key on both.
        if self.config.is_system_setting(setting) {
            let systems = values
                .iter()
                .filter_map(|value| self.config.system_named(value))
                .map(str::to_owned)
                .collect();
            return Some(Probe::Systems(systems));
        }

        Some(Probe::Constraint {
            setting: setting.to_owned(),
            domain: self.config.constraint_domain(setting),
            values: values.iter().map(|value| value.value.clone()).collect(),
        })
    }

    /// Decay's built-in `has_function` fallback, consulted only once a
    /// project's own `[probes]` entry misses.
    ///
    /// Answers only the symbols `decay_libc_db` knows are part of glibc or
    /// musl, on any of [`decay_libc_db::Cpu::ALL`] (parsed from glibc's own
    /// published ABI list, and derived from a real `zig cc` for musl —
    /// neither hand-curated), and only when the project actually has a
    /// `linux` system to ask — a project that never targets `linux` gets
    /// nothing here, same as if the database did not exist, rather than an
    /// error about an unconfigured system.
    fn builtin_has_function(&self, what: &str) -> Option<Probe> {
        if !self.config.builtin_has_function {
            return None;
        }
        const LIBCS: [(decay_libc_db::Libc, &str); 2] = [
            (decay_libc_db::Libc::Glibc, "gnu"),
            (decay_libc_db::Libc::Musl, "musl"),
        ];

        // One row per (abi, cpu) pair the database actually confirms —
        // never every abi crossed with every cpu, since presence can (and,
        // for real x86 port-I/O syscalls musl still declares everywhere,
        // does) differ by architecture; see `Probe::SystemsAndConstraint`'s
        // own doc comment for why that distinction matters.
        let rows: Vec<Vec<String>> = decay_libc_db::Cpu::ALL
            .into_iter()
            .flat_map(|cpu| {
                LIBCS
                    .into_iter()
                    .filter(move |(libc, _)| decay_libc_db::has_function(*libc, cpu, what))
                    .map(move |(_, abi)| vec![abi.to_owned(), cpu.buck2_value().to_owned()])
            })
            .collect();
        if rows.is_empty() {
            return None;
        }

        self.config
            .systems
            .contains_key("linux")
            .then(|| Probe::SystemsAndConstraint {
                systems: vec!["linux".to_owned()],
                axes: self.linux_abi_cpu_axes(),
                rows,
            })
    }

    /// The `(abi, cpu)` axes a `linux` matrix answer selects on: each buck2
    /// constraint setting paired with its known domain — what `decay.toml`
    /// already mentions, unioned with every value decay's own matrix can
    /// produce.
    fn linux_abi_cpu_axes(&self) -> Vec<(String, Vec<String>)> {
        let union = |setting: &str, extra: &[&str]| {
            let mut domain = self.config.constraint_domain(setting);
            for value in extra {
                if !domain.iter().any(|v| v == value) {
                    domain.push((*value).to_owned());
                }
            }
            domain.sort();
            domain
        };
        let cpus: Vec<&str> = decay_libc_db::Cpu::ALL
            .iter()
            .map(|c| c.buck2_value())
            .collect();
        vec![
            (probe::ABI_SETTING.to_owned(), union(probe::ABI_SETTING, &["gnu", "musl"])),
            (probe::CPU_SETTING.to_owned(), union(probe::CPU_SETTING, &cpus)),
        ]
    }

    /// Answer a [`CompileProbe`] by building it for every `linux` target in
    /// the matrix, when the configuration lets decay do that: needs
    /// `probe_with_zig` on, `zig` present, and a `linux` system to attach
    /// the answer to. An explicit `[probes]` entry for the same check has
    /// already won by the time this is reached (see [`Oracle::probe`]).
    fn compile_probe_answer(&self, probe: &CompileProbe) -> Option<Probe> {
        if let CompileProbe::Header { header } = probe
            && !probe::is_plain_header(header)
        {
            return None;
        }
        if !self.config.probe_with_zig
            || !self.config.systems.contains_key("linux")
            || !probe::zig_present()
        {
            return None;
        }

        let rows = probe::linux_rows(&mut self.probe_cache.borrow_mut(), probe);
        Some(Probe::Matrix {
            systems: vec!["linux".to_owned()],
            axes: self.linux_abi_cpu_axes(),
            rows,
        })
    }

    /// The `[sizeof]` / `[alignment]` entry for `type_name`, turned into a
    /// [`SizeAnswer`].
    fn size_answer(&self, query: SizeQuery, type_name: &str) -> Option<SizeAnswer> {
        let table = match query {
            SizeQuery::Sizeof => &self.config.sizeof,
            SizeQuery::Alignment => &self.config.alignment,
        };
        let answer = table.get(type_name)?;
        let cases = match answer {
            SizeValue::Fixed(n) => return Some(SizeAnswer::Fixed(*n)),
            SizeValue::ByConstraint(cases) => cases,
        };

        // Checked when the configuration was loaded.
        let setting = answer.setting().ok().flatten()?;
        Some(SizeAnswer::Constraint {
            setting: setting.to_owned(),
            domain: self.config.constraint_domain(setting),
            cases: cases
                .iter()
                .map(|(value, size)| (value.value.clone(), *size))
                .collect(),
        })
    }
}

impl Oracle for ConfigOracle<'_> {
    fn option(&self, name: &str) -> Option<Pinned> {
        Some(match self.project.options.get(name)? {
            OptionValue::Bool(v) => Pinned::Bool(*v),
            OptionValue::Int(v) => Pinned::Int(*v),
            OptionValue::String(v) => Pinned::Str(Rc::from(v.as_str())),
            OptionValue::List(v) => Pinned::List(v.iter().map(|s| Rc::from(s.as_str())).collect()),
        })
    }

    fn pinned_options(&self) -> Vec<String> {
        self.project.options.keys().cloned().collect()
    }

    fn probe(&self, name: &str, what: &str) -> Option<Probe> {
        self.probe_answer(&format!("{name}:{what}")).or_else(|| {
            if name == "has_function" {
                self.builtin_has_function(what)
            } else {
                None
            }
        })
    }

    fn compile_probe(&self, probe: &CompileProbe) -> Option<Probe> {
        self.compile_probe_answer(probe)
    }

    fn has_program(&self, name: &str) -> bool {
        self.config.programs.contains_key(name)
    }

    fn dependency_variables(&self, name: &str) -> Vec<(String, String)> {
        // An explicit answer in `decay.toml` overrides what importing a
        // sibling project already determined; most dependencies need neither.
        if let Some(dep) = self.config.dependencies.get(name) {
            let manual: Vec<_> = dep
                .variables()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            if !manual.is_empty() {
                return manual;
            }
        }
        self.packages
            .get(name)
            .map(|pkg| pkg.variables.clone())
            .unwrap_or_default()
    }

    fn dependency_found(&self, name: &str) -> Option<bool> {
        // Not a probe about the environment: decay is building this either
        // way, because it is another project it already imported.
        self.packages.get(name).map(|_| true)
    }

    fn dependency_variable(&self, dep: &str, variable: &str) -> Option<Probe> {
        self.probe_answer(&format!("dependency:{dep}:{variable}"))
    }

    fn type_size(&self, query: SizeQuery, type_name: &str) -> Option<SizeAnswer> {
        self.size_answer(query, type_name)
    }

    fn machine(&self, machine: obj::Machine, property: &str) -> Option<String> {
        self.machine_config(machine)
            .property(property)
            .map(str::to_owned)
    }

    fn systems(&self) -> Vec<String> {
        self.config.systems.keys().cloned().collect()
    }

    fn compilers(&self) -> Vec<String> {
        if self.config.compilers.is_empty() {
            return ["gcc", "clang", "msvc"].map(str::to_owned).to_vec();
        }
        self.config.compilers.keys().cloned().collect()
    }
}
