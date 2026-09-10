use {
    crate::probe::{
        self,
        ProbeCache, //
    },
    crate::{
        config::{
            Config,
            Machine,
            OptionScalar,
            OptionValue,
            ProbeValue,
            Project,
            SizeValue,
            Source, //
        },
        packages::Packages,
        run_command,
    },
    decay_meson_eval::{
        obj,
        oracle::{
            CompileProbe,
            CompileProbeKind,
            MatrixSystem,
            Oracle,
            Pinned,
            Probe,
            RunAnswer,
            SizeAnswer,
            SizeQuery, //
        },
    },
    std::{
        cell::RefCell,
        path::Path,
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
    /// The project's checkout — the working directory a read-only
    /// `run_command()` runs in.
    root: &'a Path,
    /// Memoised `zig cc` probe results — see [`Oracle::compile_probe`].
    probe_cache: RefCell<ProbeCache>,
}

impl<'a> ConfigOracle<'a> {
    pub fn new(
        config: &'a Config,
        project: &'a Project,
        packages: &'a Packages,
        root: &'a Path,
    ) -> Self {
        Self {
            project,
            config,
            packages,
            root,
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
        self.probe_from_value(self.config.probes.get(key)?)
    }

    /// A [`ProbeValue`] from the configuration, turned into an [`Probe`] — the
    /// grammar `[probes]` and a `[dependencies]` entry's `found` both use.
    fn probe_from_value(&self, answer: &ProbeValue) -> Option<Probe> {
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
    /// Answers only the symbols `decay_zig` knows from a bundled `abilists`
    /// export table — glibc / musl on `linux` (split on `abi`), and the
    /// FreeBSD / NetBSD libc on those systems (`cpu` only) — for any of
    /// [`decay_zig::Cpu::ALL`], none of it hand-curated. Each of those lists
    /// is *complete*, so within a system it settles found-or-not with no
    /// knob (an absent symbol is genuinely absent). Only for a configured
    /// system decay has no table for (`windows`, `darwin`, …) does the probe
    /// stay an open knob; a project targeting none of the three known
    /// systems gets nothing here.
    fn builtin_has_function(&self, what: &str) -> Option<Probe> {
        // One row per (abi, cpu) pair the database actually confirms —
        // never every abi crossed with every cpu, since presence can (and,
        // for real x86 port-I/O syscalls musl still declares everywhere,
        // does) differ by architecture; see `Probe::Matrix`'s own doc
        // comment for why that distinction matters.
        const LINUX_LIBCS: [(decay_zig::Libc, &str); 2] = [
            (decay_zig::Libc::Glibc, "gnu"),
            (decay_zig::Libc::Musl, "musl"),
        ];

        let mut systems = Vec::new();
        for (system, bsd_libc) in [
            ("linux", None),
            ("freebsd", Some(decay_zig::Libc::Freebsd)),
            ("netbsd", Some(decay_zig::Libc::Netbsd)),
        ] {
            if !self.config.systems.contains_key(system) {
                continue;
            }
            let mut rows: Vec<Vec<String>> = Vec::new();
            for cpu in decay_zig::Cpu::ALL {
                let cpu_val = cpu.buck2_value().to_owned();
                match bsd_libc {
                    // linux: one `[abi, cpu]` row per libc that confirms it.
                    None => {
                        for (libc, abi) in LINUX_LIBCS {
                            if decay_zig::has_function(libc, cpu, what) {
                                rows.push(vec![abi.to_owned(), cpu_val.clone()]);
                            }
                        }
                    }
                    // a BSD: one `[cpu]` row, no abi split.
                    Some(libc) => {
                        if decay_zig::has_function(libc, cpu, what) {
                            rows.push(vec![cpu_val]);
                        }
                    }
                }
            }
            systems.push(MatrixSystem {
                system: system.to_owned(),
                axes: self.probe_axes(system),
                rows,
            });
        }

        (!systems.is_empty()).then_some(Probe::Matrix(systems))
    }

    /// Decay's built-in `find_library()` fallback, consulted once a project's
    /// own `[dependencies]` entry misses.
    ///
    /// Settles found-or-not for *every* configured system — no
    /// `<lib>[true/false]` knob, ever. `linux` is answered from
    /// `decay_zig` (glibc's own ABI list + a real `zig cc -l` link for
    /// musl); `macos` / `freebsd` / `netbsd` / `windows` from a live `zig cc
    /// -target … -l<name>` link; the systems zig cannot host (`sunos`,
    /// `openbsd`, `android`, `fuchsia`) from `decay.toml`'s
    /// `[system_libraries]`. A name nothing confirms anywhere is not a system
    /// library — `None`, and it stays an open knob (`libselinux`, `libelf`).
    fn builtin_system_library(&self, name: &str) -> Option<Probe> {
        // An explicit `[dependencies]` mapping still wins: leave it to the
        // existing `dep:`-keyed resolution path untouched.
        if self.config.dependencies.contains_key(name) {
            return None;
        }

        // The database's own answer for `linux` (glibc's ABI list + the
        // `zig cc -l` musl probe): a fast offline pre-check that saves a
        // link.
        let db_linux_ok = decay_zig::Cpu::ALL.into_iter().any(|cpu| {
            decay_zig::has_library(decay_zig::Libc::Glibc, cpu, name)
                || decay_zig::has_library(decay_zig::Libc::Musl, cpu, name)
        });
        // MSVC ships no standalone `.lib` for a C-runtime-split library (the
        // fact `dependency('threads')` / `is_crt_provided_lib` already
        // encode), nor for `atomic` (a compiler-runtime library it covers
        // with intrinsics). A mingw hit for one of these must not also claim
        // `abi[msvc]`. ponytail: `atomic` is the one name not derivable from
        // the libc DB; revisit when a real Windows-SDK library list lands.
        let msvc_lacks = db_linux_ok || name == "atomic";

        let mut found: Vec<(String, Vec<String>)> = Vec::new();
        let mut confirmed = false;

        for system in self.config.systems.keys() {
            match probe::system_link_targets(system) {
                Some(targets) => {
                    let mut hit = false;
                    for (_abi, triple) in targets {
                        if system == "linux" && triple.contains("-gnu") && db_linux_ok {
                            hit = true; // database already confirmed it
                            continue;
                        }
                        if self.probe_cache.borrow_mut().links_library(triple, name) {
                            hit = true;
                        }
                    }
                    if !hit {
                        continue;
                    }
                    confirmed = true;
                    // `windows` was probed under `gnu` (mingw) only. A
                    // C-runtime library there is a mingw stub MSVC has no
                    // equivalent for → `gnu` only; any other library is a
                    // real Win32 import lib the Windows SDK also ships →
                    // both abis.
                    if system == "windows" && msvc_lacks {
                        found.push((system.clone(), vec!["gnu".to_owned()]));
                    } else {
                        found.push((system.clone(), Vec::new()));
                    }
                }
                None => {
                    // zig cannot host this system; only `decay.toml` can say.
                    let listed = self
                        .config
                        .system_libraries
                        .get(system)
                        .is_some_and(|libs| libs.iter().any(|l| l == name));
                    if listed {
                        confirmed = true;
                        found.push((system.clone(), Vec::new()));
                    }
                }
            }
        }

        if !confirmed {
            return None;
        }

        // Must match the `abi` axis domain `linux_abi_cpu_axes` builds — the
        // `constraint:abi` variable is shared and its first declaration wins,
        // so a different domain here would silently misindex the other
        // caller's `select()` values. `windows` rows only ever name `gnu`;
        // `msvc` never needs to be an explicit value (an abi that is not
        // `gnu` simply falls through to not-found).
        let abi_domain = self
            .linux_abi_cpu_axes()
            .into_iter()
            .find(|(setting, _)| setting == probe::ABI_SETTING)
            .map(|(_, domain)| domain)
            .unwrap_or_default();

        Some(Probe::PerSystem {
            abi: (probe::ABI_SETTING.to_owned(), abi_domain),
            found,
        })
    }

    /// The axes a compile-probe matrix answer selects on for `system`: each
    /// buck2 constraint setting paired with its known domain — what
    /// `decay.toml` already mentions, unioned with every value decay's own
    /// matrix can produce. `linux` splits on `abi` (glibc vs musl); the BSDs
    /// have only a `cpu` axis.
    fn probe_axes(&self, system: &str) -> Vec<(String, Vec<String>)> {
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
        let cpus: Vec<&str> = decay_zig::Cpu::ALL
            .iter()
            .map(|c| c.buck2_value())
            .collect();
        let cpu_axis = (
            probe::CPU_SETTING.to_owned(),
            union(probe::CPU_SETTING, &cpus),
        );
        match system {
            "linux" => vec![
                (
                    probe::ABI_SETTING.to_owned(),
                    union(probe::ABI_SETTING, &["gnu", "musl"]),
                ),
                cpu_axis,
            ],
            _ => vec![cpu_axis],
        }
    }

    /// The `(abi, cpu)` axes a `linux` answer selects on — the abi-split
    /// callers ([`Self::builtin_has_function`], [`Self::builtin_system_library`])
    /// still want exactly this shape.
    fn linux_abi_cpu_axes(&self) -> Vec<(String, Vec<String>)> {
        self.probe_axes("linux")
    }

    /// Answer a [`CompileProbe`] by building it with `zig` for every target
    /// in the matrix, on each system decay can probe fully
    /// ([`probe::PROBE_SYSTEMS`]) that the configuration actually uses. An
    /// explicit `[probes]` entry for the same check has already won by the
    /// time this is reached (see [`Oracle::probe`]).
    fn compile_probe_answer(&self, probe: &CompileProbe) -> Option<Probe> {
        let header = match &probe.kind {
            CompileProbeKind::Header { header } | CompileProbeKind::HeaderSymbol { header, .. } => {
                Some(header)
            }
            _ => None,
        };
        if let Some(header) = header
            && !probe::is_plain_header(header)
        {
            return None;
        }

        let mut systems = Vec::new();
        for system in probe::PROBE_SYSTEMS {
            if !self.config.systems.contains_key(system) {
                continue;
            }
            let rows = probe::probe_rows(&mut self.probe_cache.borrow_mut(), probe, system);
            let (axes, rows) = collapse_full_axes(self.probe_axes(system), rows);
            systems.push(MatrixSystem {
                system: system.to_owned(),
                axes,
                rows,
            });
        }
        (!systems.is_empty()).then_some(Probe::Matrix(systems))
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
            OptionValue::ByConstraint { cases, default } => {
                // `cases` is non-empty and shares one setting — both checked
                // when the configuration was loaded.
                let setting = cases[0].0.setting.as_str();
                let mut domain = self.config.constraint_domain(setting);
                for (value, _) in cases {
                    if !domain.contains(&value.value) {
                        domain.push(value.value.clone());
                    }
                }
                domain.sort();
                Pinned::ByConstraint {
                    setting: setting.to_owned(),
                    domain,
                    cases: cases
                        .iter()
                        .map(|(v, s)| (v.value.clone(), Box::new(scalar_pinned(s))))
                        .collect(),
                    default: default.as_ref().map(|s| Box::new(scalar_pinned(s))),
                }
            }
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

    fn dependency_found(&self, name: &str) -> Option<Probe> {
        // Not a probe about the environment: decay is building this either
        // way, because it is another project it already imported.
        if self.packages.get(name).is_some() {
            return Some(Probe::Fixed(true));
        }
        // A `[dependencies]` entry is the configuration asserting the
        // dependency is satisfied — unconditionally, or only where a
        // constraint holds (a library that exists on one OS). Either way it is
        // settled, not a knob.
        let dep = self.config.dependencies.get(name)?;
        Some(match dep.found() {
            Some(answer) => self.probe_from_value(answer)?,
            None => Probe::Fixed(true),
        })
    }

    fn system_library(&self, name: &str) -> Option<Probe> {
        self.builtin_system_library(name)
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

    fn run_command(&self, argv: &[String]) -> Option<RunAnswer> {
        // 1. An explicit per-project answer wins.
        if let Some(c) = self.project.commands.get(&argv.join(" ")) {
            return Some(RunAnswer {
                code: c.returncode,
                stdout: c.stdout.clone(),
                stderr: c.stderr.clone(),
            });
        }
        // 2. `git describe` follows from the ref decay already pinned.
        if let [cmd, sub, ..] = argv
            && cmd == "git"
            && sub == "describe"
            && let Source::Git { reference, .. } = &self.project.source
        {
            return Some(run_command::git_describe(reference, argv));
        }
        // 3. A read-only command over the pinned checkout, else refused.
        run_command::run_readonly(self.root, argv)
    }
}

/// A compile-probe matrix axis: a constraint setting and its value domain.
type Axis = (String, Vec<String>);
/// Matrix rows: each row picks one value per axis, in axis order.
type MatrixRows = Vec<Vec<String>>;

/// Drop any axis of a compile-probe matrix whose entire real domain is
/// covered for every combination of the other axes — the probe compiled
/// *everywhere* along it, so selecting on it distinguishes nothing. This
/// subsumes "`linux` `gnu` and `musl` agree, drop the `abi` axis": grouping
/// the `[abi, cpu]` rows by cpu, an `abi` domain that shows up in full for
/// every cpu present is exactly that.
///
/// With every axis dropped the system's answer is an unconditional "true
/// here" (`rows == [[]]`); when every probed system reaches that,
/// [`Selects::simplify`] folds the whole probe to `true` and no `select()`
/// is emitted. The `constraint_var` fallback value (`ANY_OTHER`) is what
/// otherwise keeps such a probe non-tautological forever — every attribute
/// touching it would grow an `abi` — then `cpu` — `select()`.
fn collapse_full_axes(mut axes: Vec<Axis>, mut rows: MatrixRows) -> (Vec<Axis>, MatrixRows) {
    if rows.is_empty() {
        return (axes, rows);
    }
    'again: loop {
        for i in 0..axes.len() {
            let domain: std::collections::BTreeSet<&str> =
                axes[i].1.iter().map(String::as_str).collect();
            let mut groups: std::collections::HashMap<
                Vec<String>,
                std::collections::BTreeSet<String>,
            > = std::collections::HashMap::new();
            for row in &rows {
                if row.len() != axes.len() {
                    return (axes, rows);
                }
                let others: Vec<String> = row
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, v)| v.clone())
                    .collect();
                groups.entry(others).or_default().insert(row[i].clone());
            }
            let covered = groups.values().all(|vals| {
                vals.len() == domain.len() && vals.iter().all(|v| domain.contains(v.as_str()))
            });
            if covered {
                axes.remove(i);
                let mut seen = std::collections::BTreeSet::new();
                rows = rows
                    .into_iter()
                    .map(|mut r| {
                        r.remove(i);
                        r
                    })
                    .filter(|r| seen.insert(r.clone()))
                    .collect();
                continue 'again;
            }
        }
        break;
    }
    (axes, rows)
}

fn scalar_pinned(scalar: &OptionScalar) -> Pinned {
    match scalar {
        OptionScalar::Bool(v) => Pinned::Bool(*v),
        OptionScalar::Int(v) => Pinned::Int(*v),
        OptionScalar::String(v) => Pinned::Str(Rc::from(v.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ax(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(s, d)| (s.to_string(), d.iter().map(|v| v.to_string()).collect()))
            .collect()
    }
    fn rows(rs: &[&[&str]]) -> Vec<Vec<String>> {
        rs.iter()
            .map(|r| r.iter().map(|v| v.to_string()).collect())
            .collect()
    }

    #[test]
    fn abi_axis_dropped_when_gnu_equals_musl() {
        // Partial cpu coverage, but abi never discriminates → abi axis goes,
        // cpu axis stays (only 2 of its domain present).
        let (axes, r) = collapse_full_axes(
            ax(&[
                ("abi", &["gnu", "musl"]),
                ("cpu", &["x86_64", "arm64", "riscv64"]),
            ]),
            rows(&[
                &["gnu", "x86_64"],
                &["musl", "x86_64"],
                &["gnu", "arm64"],
                &["musl", "arm64"],
            ]),
        );
        assert_eq!(axes.len(), 1);
        assert_eq!(axes[0].0, "cpu");
        assert_eq!(r, rows(&[&["x86_64"], &["arm64"]]));
    }

    #[test]
    fn abi_axis_kept_when_musl_missing_for_a_cpu() {
        let before = (
            ax(&[("abi", &["gnu", "musl"]), ("cpu", &["x86_64", "arm64"])]),
            rows(&[&["gnu", "x86_64"], &["musl", "x86_64"], &["gnu", "arm64"]]),
        );
        let after = collapse_full_axes(before.0.clone(), before.1.clone());
        assert_eq!(after, before);
    }

    #[test]
    fn full_coverage_collapses_to_always() {
        let (axes, r) = collapse_full_axes(
            ax(&[("cpu", &["x86_64", "arm64"])]),
            rows(&[&["x86_64"], &["arm64"]]),
        );
        assert!(axes.is_empty());
        assert_eq!(r, vec![Vec::<String>::new()]);
    }

    #[test]
    fn partial_coverage_keeps_axis() {
        let before = (ax(&[("cpu", &["x86_64", "arm64"])]), rows(&[&["x86_64"]]));
        let after = collapse_full_axes(before.0.clone(), before.1.clone());
        assert_eq!(after, before);
    }

    #[test]
    fn one_full_axis_dropped_the_other_kept() {
        // abi fully covered for the one cpu present; cpu not fully covered.
        let (axes, r) = collapse_full_axes(
            ax(&[
                (probe::ABI_SETTING, &["gnu", "musl"]),
                ("cpu", &["x86_64", "arm64"]),
            ]),
            rows(&[&["gnu", "x86_64"], &["musl", "x86_64"]]),
        );
        assert_eq!(axes.len(), 1);
        assert_eq!(axes[0].0, "cpu");
        assert_eq!(r, rows(&[&["x86_64"]]));
    }
}
