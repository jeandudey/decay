use {
    eyre::{
        Context,
        eyre, //
    },
    serde::Deserialize,
    sha2::{
        Digest,
        Sha256, //
    },
    std::{
        collections::BTreeMap,
        fmt, fs,
        path::{
            Path,
            PathBuf, //
        },
        str::FromStr,
    },
    url::Url,
};

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Where generated build files are written, relative to the repository
    /// root.
    pub third_party_dir: PathBuf,
    /// An optional Meson `subprojects`-style directory containing local
    /// `.wrap` files.  A `[[project]] wrap = "name"` uses `name.wrap` from
    /// here when present, before falling back to wrapdb.
    #[serde(default, alias = "local_wraps")]
    pub wrap_dir: Option<PathBuf>,
    /// The systems a build may target, mapped to the build-file label that
    /// selects each one.
    #[serde(default)]
    pub systems: BTreeMap<String, String>,
    /// The compilers a build may use, mapped the same way. Left empty, the
    /// importer generates its own constraints.
    #[serde(default)]
    pub compilers: BTreeMap<String, String>,
    /// Targets that already provide a dependency the meson build looks up
    /// outside itself, keyed by the name meson uses (`dependency('x11')`,
    /// `cc.find_library('dl')`).
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencyValue>,
    /// Binary targets for the tools a build runs, keyed by the name meson looks
    /// up (`find_program('doxygen')`).
    ///
    /// A build graph cannot reach outside itself for a tool, so a program is
    /// found only when it is a script in the project or is named here. Anything
    /// else is absent, and whatever the build would have done with it is left
    /// out.
    #[serde(default)]
    pub programs: BTreeMap<String, String>,
    /// Answers to compiler probes the importer would otherwise have to leave
    /// open, keyed by the check and its argument (`has_function:dlvsym`).
    ///
    /// A probe listed here follows from a constraint the build already has, so
    /// it selects on that instead of carrying a second constraint that would
    /// always have to be set to agree with it.
    #[serde(default)]
    pub probes: BTreeMap<String, ProbeValue>,
    /// Answers to `cc.sizeof()`, keyed by the type name it is passed
    /// (`sizeof."void*"`). The value is either a single integer, when the
    /// size is the same everywhere, or a table of constraint value to
    /// integer, when it follows from the target — a CPU whose pointer width
    /// differs, typically. A build graph cannot run the compiler, and the
    /// size is a concrete number a generated header bakes in, so a type a
    /// project asks about must be answered here.
    #[serde(default)]
    pub sizeof: BTreeMap<String, SizeValue>,
    /// Answers to `cc.alignment()`, in the same shape as [`Self::sizeof`].
    #[serde(default)]
    pub alignment: BTreeMap<String, SizeValue>,
    /// Whether `has_function` falls back to decay's built-in glibc/musl
    /// symbol database (see `decay_libc_db`) when `[probes]` has no entry
    /// for it.
    ///
    /// An explicit `[probes]` entry for the same check always wins over the
    /// built-in answer; this only turns the fallback off entirely, for a
    /// project that wants every `has_function` left open regardless.
    #[serde(default = "default_true")]
    pub builtin_has_function: bool,
    /// Whether `cc.find_library()` falls back to decay's built-in database of
    /// libraries the C runtime splits out (see `decay_libc_db`) when
    /// `[dependencies]` has no entry for the name.
    ///
    /// An explicit `[dependencies]` mapping always wins; this only turns the
    /// fallback off entirely, for a project that wants every `find_library()`
    /// left open regardless.
    #[serde(default = "default_true")]
    pub builtin_system_library: bool,
    /// `cc.find_library()` answers for the systems `zig cc` cannot link-probe
    /// (no bundled libc: `sunos`/`illumos`, `openbsd`, `android`, `fuchsia`),
    /// keyed by system name, listing the library names that resolve there.
    ///
    /// The libc database and `zig cc` link probes answer every other
    /// configured system on their own; this is only for the ones neither can
    /// reach, and it takes the place of an open `<lib>[true/false]` knob —
    /// there is none. A library not listed for such a system is settled
    /// not-found there.
    #[serde(default)]
    pub system_libraries: BTreeMap<String, Vec<String>>,
    /// Whether `cc.has_header` / `cc.has_type` / `cc.compiles` are answered
    /// by building the probe with a live `zig cc` for every target in the
    /// configured matrix, instead of leaving each an open knob. Needs `zig`
    /// on `PATH`; without it this has no effect. An explicit `[probes]`
    /// entry for the same check always wins, and a probe carrying an
    /// `args:` / `dependencies:` the importer cannot replay is left open
    /// regardless.
    #[serde(default = "default_true")]
    pub probe_with_zig: bool,
    /// Global options applied to every project unless overridden by that project.
    #[serde(default)]
    pub options: BTreeMap<String, OptionValue>,
    #[serde(rename = "project")]
    pub projects: Vec<Project>,
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn from_file(path: impl AsRef<Path>) -> eyre::Result<Self> {
        let path = path.as_ref();
        let file = fs::read_to_string(path).wrap_err("Failed to load configuration file")?;
        let mut config: Self =
            toml::from_str(&file).wrap_err("Failed to parse configuration file")?;
        if let Some(dir) = &mut config.wrap_dir
            && dir.is_relative()
        {
            *dir = path.parent().unwrap_or_else(|| Path::new(".")).join(&dir);
        }
        config.check()?;
        Ok(config)
    }

    fn check(&mut self) -> eyre::Result<()> {
        for project in &mut self.projects {
            for (key, val) in &self.options {
                project
                    .options
                    .entry(key.clone())
                    .or_insert_with(|| val.clone());
            }
        }

        for project in &self.projects {
            for (name, val) in &project.options {
                let check = || -> eyre::Result<()> {
                    let Some(setting) = val.setting()? else {
                        return Ok(());
                    };
                    if self.is_system_setting(setting) {
                        return Err(eyre!(
                            "`{setting}` is the constraint `[systems]` already selects on; \
                             key the option on `abi`, `cpu` or the compiler instead"
                        ));
                    }
                    Ok(())
                };
                check().wrap_err_with(|| format!("Option `{name}` cannot be pinned"))?;
            }
        }

        for (probe, answer) in &self.probes {
            let check = || -> eyre::Result<()> {
                let Some(setting) = answer.setting()? else {
                    return Ok(());
                };
                if !self.is_system_setting(setting) {
                    return Ok(());
                }
                // The system is already a variable of its own, and an answer
                // that names the same constraint has to go through it. That
                // only works for the values `[systems]` maps.
                for value in answer.values() {
                    if self.system_named(value).is_none() {
                        return Err(eyre!(
                            "`{value}` is a value of the constraint `[systems]` selects on, \
                             but no system there maps to it"
                        ));
                    }
                }
                Ok(())
            };
            check().wrap_err_with(|| format!("Probe `{probe}` cannot be answered"))?;
        }

        for (section, table) in [("sizeof", &self.sizeof), ("alignment", &self.alignment)] {
            for (type_name, answer) in table {
                let check = || -> eyre::Result<()> {
                    let Some(setting) = answer.setting()? else {
                        return Ok(());
                    };
                    if self.is_system_setting(setting) {
                        return Err(eyre!(
                            "`{setting}` is the constraint `[systems]` already selects on; \
                             a size follows from the CPU or ABI, not the operating system"
                        ));
                    }
                    Ok(())
                };
                check().wrap_err_with(|| {
                    format!("`[{section}]` entry `{type_name}` cannot be answered")
                })?;
            }
        }
        Ok(())
    }

    /// Whether `setting` is the constraint `[systems]` selects on.
    pub fn is_system_setting(&self, setting: &str) -> bool {
        let prefix = format!("{setting}[");
        self.systems
            .values()
            .any(|label| label.starts_with(&prefix))
    }

    /// The system `[systems]` maps onto exactly this constraint value.
    pub fn system_named(&self, value: &ConstraintValue) -> Option<&str> {
        let label = value.to_string();
        self.systems
            .iter()
            .find(|(_, mapped)| **mapped == label)
            .map(|(system, _)| system.as_str())
    }

    /// Every value of `setting` the configuration mentions.
    ///
    /// A constraint declared elsewhere has values the importer never sees, so
    /// what it can tell apart is exactly what it was told about. `[probes]`,
    /// `[sizeof]` and `[alignment]` all feed one shared constraint variable
    /// per setting, so its domain is the union of what they each name.
    pub fn constraint_domain(&self, setting: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let probe_values = self.probes.values().flat_map(ProbeValue::values);
        let size_values = self
            .sizeof
            .values()
            .chain(self.alignment.values())
            .flat_map(SizeValue::constraint_values);
        for value in probe_values.chain(size_values) {
            if value.setting == setting && !out.contains(&value.value) {
                out.push(value.value.clone());
            }
        }
        out.sort();
        out
    }
}

/// A target that answers a `dependency()` lookup, as the configuration states
/// it.
///
/// The plain string form (`x11 = "//third-party/system:X11"`) is what most
/// entries need: a label to use once the dependency is found. A table form
/// adds the `pkg-config` variables the build reads off it, for a dependency
/// like `iso-codes` that a project queries rather than links.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DependencyValue {
    Target(String),
    Full {
        target: Option<String>,
        #[serde(default)]
        variables: BTreeMap<String, String>,
    },
}

impl DependencyValue {
    /// The build-file label to use once the dependency is found, if any.
    pub fn target(&self) -> Option<&str> {
        match self {
            DependencyValue::Target(target) => Some(target),
            DependencyValue::Full { target, .. } => target.as_deref(),
        }
    }

    /// The `pkg-config` variables the configuration answers for this
    /// dependency.
    pub fn variables(&self) -> impl Iterator<Item = (&str, &str)> {
        let variables = match self {
            DependencyValue::Target(_) => None,
            DependencyValue::Full { variables, .. } => Some(variables),
        };
        variables
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// What a compiler probe answers, as the configuration states it.
#[derive(Debug)]
pub enum ProbeValue {
    /// The same answer in every configuration.
    Fixed(bool),
    /// True exactly when this constraint value is selected.
    One(ConstraintValue),
    /// True when any one of these is, all values of the same setting.
    Any(Vec<ConstraintValue>),
}

impl ProbeValue {
    /// The constraint values this answer holds for.
    pub fn values(&self) -> &[ConstraintValue] {
        match self {
            ProbeValue::Fixed(_) => &[],
            ProbeValue::One(value) => std::slice::from_ref(value),
            ProbeValue::Any(values) => values,
        }
    }

    /// The setting every value belongs to, which they all must share: the
    /// executor turns the answer into a choice of one constraint, and two
    /// settings are two independent choices.
    pub fn setting(&self) -> eyre::Result<Option<&str>> {
        let mut values = self.values().iter();
        let Some(first) = values.next() else {
            return Ok(None);
        };
        for other in values {
            if other.setting != first.setting {
                return Err(eyre!(
                    "`{}` and `{}` are values of different constraints, which \
                     cannot both answer one probe",
                    first,
                    other
                ));
            }
        }
        Ok(Some(&first.setting))
    }
}

impl<'de> Deserialize<'de> for ProbeValue {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Parsed in a second pass rather than by an untagged enum of the final
        // types, which would report only that nothing matched.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Fixed(bool),
            One(String),
            Any(Vec<String>),
        }

        let parse = |text: &str| text.parse().map_err(serde::de::Error::custom);
        Ok(match Raw::deserialize(de)? {
            Raw::Fixed(settled) => ProbeValue::Fixed(settled),
            Raw::One(text) => ProbeValue::One(parse(&text)?),
            Raw::Any(texts) => ProbeValue::Any(
                texts
                    .iter()
                    .map(|text| parse(text))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}

/// What `cc.sizeof()` or `cc.alignment()` answers for one type, as the
/// configuration states it.
#[derive(Debug)]
pub enum SizeValue {
    /// The same number of bytes in every configuration.
    Fixed(i64),
    /// One number per constraint value — a CPU map, typically, for a type
    /// whose width follows from the target. Every key is a value of the same
    /// constraint setting.
    ByConstraint(Vec<(ConstraintValue, i64)>),
}

impl SizeValue {
    /// The constraint values this answer branches on, none when it is fixed.
    pub fn constraint_values(&self) -> impl Iterator<Item = &ConstraintValue> {
        let cases: &[(ConstraintValue, i64)] = match self {
            SizeValue::Fixed(_) => &[],
            SizeValue::ByConstraint(cases) => cases,
        };
        cases.iter().map(|(value, _)| value)
    }

    /// The setting every key belongs to, which they all must share: the
    /// executor turns the answer into a choice of one constraint.
    pub fn setting(&self) -> eyre::Result<Option<&str>> {
        let mut values = self.constraint_values();
        let Some(first) = values.next() else {
            return Ok(None);
        };
        for other in values {
            if other.setting != first.setting {
                return Err(eyre!(
                    "`{first}` and `{other}` are values of different constraints, which \
                     cannot both answer one size"
                ));
            }
        }
        Ok(Some(&first.setting))
    }
}

impl<'de> Deserialize<'de> for SizeValue {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Fixed(i64),
            ByConstraint(BTreeMap<String, i64>),
        }

        Ok(match Raw::deserialize(de)? {
            Raw::Fixed(n) => SizeValue::Fixed(n),
            Raw::ByConstraint(cases) => {
                let mut out = Vec::with_capacity(cases.len());
                for (key, size) in cases {
                    let value = key.parse().map_err(serde::de::Error::custom)?;
                    out.push((value, size));
                }
                SizeValue::ByConstraint(out)
            }
        })
    }
}

/// One value of one constraint, written the way a `select()` key is:
/// `prelude//abi/constraints:abi[gnu]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintValue {
    /// Label of the constraint setting, without the value.
    pub setting: String,
    pub value: String,
}

impl FromStr for ConstraintValue {
    type Err = eyre::Report;

    fn from_str(text: &str) -> eyre::Result<Self> {
        let rest = text.strip_suffix(']').ok_or_else(|| {
            eyre!("`{text}` is not a constraint value; write it as `//some:setting[value]`")
        })?;
        let (setting, value) = rest.split_once('[').ok_or_else(|| {
            eyre!("`{text}` is not a constraint value; write it as `//some:setting[value]`")
        })?;
        Ok(Self {
            setting: setting.to_owned(),
            value: value.to_owned(),
        })
    }
}

impl fmt::Display for ConstraintValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]", self.setting, self.value)
    }
}

#[derive(Debug)]
pub struct Project {
    pub source: Source,
    /// Options pinned to a fixed value.
    ///
    /// Anything left out stays a build-time choice, which is the point: most
    /// projects should pin nothing here.
    pub options: BTreeMap<String, OptionValue>,
    pub host_machine: Machine,
    pub build_machine: Machine,
    /// Other projects in this file that must be imported before this one,
    /// named the way `short_name()` spells them (`glib`, `graphene`).
    ///
    /// A `dependency()` here resolves against a sibling only once that sibling
    /// has been executed, so a consumer has to name every provider it looks up.
    /// A project that names nothing is taken to be independent and runs in the
    /// first wave alongside every other such project; `-j` then decides how many
    /// of them go at once.
    pub depends: Vec<String>,
}

impl Project {
    /// The name a sibling's `depends` and `dependency()` know this project
    /// by: the repository's last path segment for a git project, the wrap's
    /// name for a wrap.
    pub fn short_name(&self) -> String {
        self.source.short_name()
    }
}

/// Where a project's sources come from, as one `[[project]]` entry names it:
/// a git checkout (`repo`, plus exactly one of `branch`, `tag`, or `rev`),
/// or a meson wrap resolved against wrapdb
/// (`wrap`, optionally pinned to a `version`).
#[derive(Debug, Clone)]
pub enum Source {
    Git {
        repo: Repo,
        reference: GitReference,
    },
    Wrap {
        name: String,
        version: Option<String>,
    },
}

/// The spelling used to select a git checkout.  These are deliberately
/// distinct: a repository can have both a branch and a tag called `v1`, and
/// Cargo's `branch`/`tag`/`rev` keys promise to select different namespaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitReference {
    Branch(String),
    Tag(String),
    Rev(String),
}

impl GitReference {
    pub fn value(&self) -> &str {
        match self {
            Self::Branch(value) | Self::Tag(value) | Self::Rev(value) => value,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Branch(_) => "branch",
            Self::Tag(_) => "tag",
            Self::Rev(_) => "rev",
        }
    }

    /// The precise remote ref to resolve. `rev` intentionally remains Cargo's
    /// general git-revision syntax, while branch and tag never fall through to
    /// each other's namespace.
    pub fn remote_ref(&self) -> String {
        match self {
            Self::Branch(value) => format!("refs/heads/{value}"),
            Self::Tag(value) => format!("refs/tags/{value}"),
            Self::Rev(value) => value.clone(),
        }
    }
}

impl Source {
    pub fn short_name(&self) -> String {
        match self {
            Source::Git { repo, .. } => repo.short_name(),
            Source::Wrap { name, .. } => name.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for Project {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Parsed in a second pass rather than by an untagged enum of the final
        // types, which would report only that nothing matched.
        #[derive(Deserialize)]
        struct Raw {
            repo: Option<Repo>,
            rev: Option<String>,
            branch: Option<String>,
            tag: Option<String>,
            wrap: Option<String>,
            version: Option<String>,
            #[serde(default)]
            options: BTreeMap<String, OptionValue>,
            #[serde(default)]
            host_machine: Machine,
            #[serde(default)]
            build_machine: Machine,
            #[serde(default)]
            depends: Vec<String>,
        }

        let raw = Raw::deserialize(de)?;
        let reference = match (raw.branch, raw.tag, raw.rev) {
            (Some(branch), None, None) => Some(GitReference::Branch(branch)),
            (None, Some(tag), None) => Some(GitReference::Tag(tag)),
            (None, None, Some(rev)) => Some(GitReference::Rev(rev)),
            (None, None, None) => None,
            _ => {
                return Err(serde::de::Error::custom(
                    "a git project may specify only one of `branch`, `tag`, or `rev`",
                ));
            }
        };
        let source = match (raw.repo, reference, raw.wrap, raw.version) {
            (Some(repo), Some(reference), None, None) => Source::Git { repo, reference },
            (None, None, Some(name), version) => Source::Wrap { name, version },
            (Some(_), Some(_), None, Some(_)) => {
                return Err(serde::de::Error::custom(
                    "`version` only means something for a `wrap` entry, not a git project",
                ));
            }
            _ => {
                return Err(serde::de::Error::custom(
                    "a project is either a git checkout (`repo` and one of `branch`, `tag`, or `rev`) or a meson wrap \
                     (`wrap`, and optionally the wrapdb `version` to pin), not a mix of both \
                     or neither",
                ));
            }
        };
        Ok(Project {
            source,
            options: raw.options,
            host_machine: raw.host_machine,
            build_machine: raw.build_machine,
            depends: raw.depends,
        })
    }
}

/// A full, lowercase 40-character commit hash, as opposed to a branch or tag
/// name — the only thing the generated build fetches by, so the difference
/// matters here and in [`crate::lock`], which is what turns the latter into
/// the former.
pub fn is_full_sha(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Repo(pub Url);

impl Repo {
    pub fn ident(&self) -> eyre::Result<String> {
        let name: String = self.0
            .path_segments()
            .ok_or(eyre!("Repository URL doesn't contain segments, this probably shouldn't be an error if someone hosts a git repository at the URL root"))?
            .collect();
        let hash = Sha256::digest(self.0.to_string());
        Ok(format!("{name}-{}", hex::encode(&hash[..8])))
    }

    /// The last path segment, which is what the project is usually called.
    pub fn short_name(&self) -> String {
        self.0
            .path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or("project")
            .trim_end_matches(".git")
            .to_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    String(String),
    List(Vec<String>),
    /// One scalar per value of a constraint the build already selects on,
    /// written as a table of `select()` key to value — the option
    /// counterpart of a `[sizeof]` table. A `DEFAULT` key sets the value for
    /// every other constraint value.
    ByConstraint {
        cases: Vec<(ConstraintValue, OptionScalar)>,
        default: Option<OptionScalar>,
    },
}

/// A plain option value, as it appears inside an [`OptionValue::ByConstraint`]
/// table.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum OptionScalar {
    Bool(bool),
    Int(i64),
    String(String),
}

impl OptionValue {
    /// The constraint values a by-constraint pin branches on, none otherwise.
    fn constraint_values(&self) -> impl Iterator<Item = &ConstraintValue> {
        let cases: &[(ConstraintValue, OptionScalar)] = match self {
            OptionValue::ByConstraint { cases, .. } => cases,
            _ => &[],
        };
        cases.iter().map(|(value, _)| value)
    }

    /// The one setting every key of a by-constraint pin must share.
    pub fn setting(&self) -> eyre::Result<Option<&str>> {
        let mut values = self.constraint_values();
        let Some(first) = values.next() else {
            return Ok(None);
        };
        for other in values {
            if other.setting != first.setting {
                return Err(eyre!(
                    "`{first}` and `{other}` are values of different constraints, which \
                     cannot both answer one option"
                ));
            }
        }
        Ok(Some(&first.setting))
    }
}

impl<'de> Deserialize<'de> for OptionValue {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Int(i64),
            String(String),
            List(Vec<String>),
            Map(BTreeMap<String, OptionScalar>),
        }

        Ok(match Raw::deserialize(de)? {
            Raw::Bool(v) => OptionValue::Bool(v),
            Raw::Int(v) => OptionValue::Int(v),
            Raw::String(v) => OptionValue::String(v),
            Raw::List(v) => OptionValue::List(v),
            Raw::Map(map) => {
                let mut cases = Vec::new();
                let mut default = None;
                for (key, val) in map {
                    if key == "DEFAULT" {
                        default = Some(val);
                    } else {
                        cases.push((key.parse().map_err(serde::de::Error::custom)?, val));
                    }
                }
                if cases.is_empty() {
                    return Err(serde::de::Error::custom(
                        "an option table needs at least one `//setting:name[value]` key",
                    ));
                }
                OptionValue::ByConstraint { cases, default }
            }
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Machine {
    pub system: Option<String>,
    pub cpu_family: Option<String>,
    pub cpu: Option<String>,
    pub endian: Option<String>,
}

impl Machine {
    pub fn property(&self, name: &str) -> Option<&str> {
        match name {
            "system" => self.system.as_deref(),
            "cpu_family" => self.cpu_family.as_deref(),
            "cpu" => self.cpu.as_deref(),
            "endian" => self.endian.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(body: &str) -> eyre::Result<Config> {
        let toml = format!(
            "third_party_dir = \"tp\"\n\
             [systems]\n\
             linux = \"prelude//os/constraints:os[linux]\"\n\
             {body}\n\
             [[project]]\n\
             repo = \"https://example.com/x/glib.git\"\n\
             rev = \"0000000000000000000000000000000000000000\"\n"
        );
        let mut cfg: Config = toml::from_str(&toml)?;
        cfg.check()?;
        Ok(cfg)
    }

    #[test]
    fn global_options_inherited_and_overridden() {
        let toml = "third_party_dir = \"tp\"\n\
             [options]\n\
             buildtype = \"release\"\n\
             optimization = \"3\"\n\
             default_library = \"shared\"\n\
             \n\
             [[project]]\n\
             repo = \"https://example.com/x/liba.git\"\n\
             rev = \"0000000000000000000000000000000000000000\"\n\
             \n\
             [[project]]\n\
             repo = \"https://example.com/x/libb.git\"\n\
             rev = \"1111111111111111111111111111111111111111\"\n\
             options.optimization = \"2\"\n\
             options.tests = false\n";
        let mut cfg: Config = toml::from_str(toml).unwrap();
        cfg.check().unwrap();

        assert_eq!(cfg.projects.len(), 2);

        // Project A inherits all global options
        let p_a = &cfg.projects[0];
        assert_eq!(
            p_a.options.get("buildtype"),
            Some(&OptionValue::String("release".into()))
        );
        assert_eq!(
            p_a.options.get("optimization"),
            Some(&OptionValue::String("3".into()))
        );
        assert_eq!(
            p_a.options.get("default_library"),
            Some(&OptionValue::String("shared".into()))
        );
        assert_eq!(p_a.options.get("tests"), None);

        // Project B inherits buildtype and default_library, overrides optimization, adds tests
        let p_b = &cfg.projects[1];
        assert_eq!(
            p_b.options.get("buildtype"),
            Some(&OptionValue::String("release".into()))
        );
        assert_eq!(
            p_b.options.get("optimization"),
            Some(&OptionValue::String("2".into()))
        );
        assert_eq!(
            p_b.options.get("default_library"),
            Some(&OptionValue::String("shared".into()))
        );
        assert_eq!(p_b.options.get("tests"), Some(&OptionValue::Bool(false)));
    }

    #[test]
    fn option_parses_scalar_and_by_constraint() {
        let cfg = config(
            "[[project]]\n\
             repo = \"https://example.com/x/glib.git\"\n\
             rev = \"0000000000000000000000000000000000000000\"\n\
             options.tests = false\n\
             options.force_posix_threads = { \"prelude//abi/constraints:abi[gnu]\" = true, \"DEFAULT\" = false }\n",
        )
        .unwrap();

        let opts = &cfg.projects[0].options;
        assert_eq!(opts.get("tests"), Some(&OptionValue::Bool(false)));
        let OptionValue::ByConstraint { cases, default } = &opts["force_posix_threads"] else {
            panic!("expected a constraint map");
        };
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].0.to_string(), "prelude//abi/constraints:abi[gnu]");
        assert_eq!(cases[0].1, OptionScalar::Bool(true));
        assert_eq!(*default, Some(OptionScalar::Bool(false)));
        assert_eq!(
            opts["force_posix_threads"].setting().unwrap(),
            Some("prelude//abi/constraints:abi")
        );
    }

    #[test]
    fn option_on_system_setting_is_rejected() {
        let err = config(
            "[[project]]\n\
             repo = \"https://example.com/x/glib.git\"\n\
             rev = \"0000000000000000000000000000000000000000\"\n\
             options.force_posix_threads = { \"prelude//os/constraints:os[windows]\" = false, \"DEFAULT\" = true }\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("force_posix_threads"), "{err}");
    }

    #[test]
    fn sizeof_parses_fixed_and_by_constraint() {
        let cfg = config(
            "[sizeof]\n\
             \"int\" = 4\n\
             \"void*\" = { \"prelude//cpu/constraints:cpu[x86_64]\" = 8, \"prelude//cpu/constraints:cpu[x86]\" = 4 }\n",
        )
        .unwrap();

        assert!(matches!(cfg.sizeof["int"], SizeValue::Fixed(4)));
        let SizeValue::ByConstraint(cases) = &cfg.sizeof["void*"] else {
            panic!("expected a constraint map");
        };
        assert_eq!(cases.len(), 2);
        assert_eq!(
            cfg.sizeof["void*"].setting().unwrap(),
            Some("prelude//cpu/constraints:cpu")
        );
    }

    #[test]
    fn constraint_domain_unions_probes_and_sizes() {
        let cfg = config(
            "[probes]\n\
             \"compiles:NEON\" = \"prelude//cpu/constraints:cpu[arm64]\"\n\
             [sizeof]\n\
             \"void*\" = { \"prelude//cpu/constraints:cpu[x86_64]\" = 8 }\n\
             [alignment]\n\
             \"long\" = { \"prelude//cpu/constraints:cpu[x86]\" = 4 }\n",
        )
        .unwrap();

        assert_eq!(
            cfg.constraint_domain("prelude//cpu/constraints:cpu"),
            ["arm64", "x86", "x86_64"]
        );
    }

    #[test]
    fn sizeof_on_the_system_constraint_is_rejected() {
        let err = config(
            "[sizeof]\n\
             \"void*\" = { \"prelude//os/constraints:os[linux]\" = 8 }\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("already selects on"));
    }

    #[test]
    fn a_project_entry_can_be_a_wrap() {
        let toml = "third_party_dir = \"tp\"\n\
             [[project]]\n\
             wrap = \"zlib\"\n\
             \n\
             [[project]]\n\
             wrap = \"expat\"\n\
             version = \"2.7.1-1\"\n";
        let cfg: Config = toml::from_str(toml).unwrap();

        assert_eq!(cfg.projects.len(), 2);
        assert!(matches!(
            &cfg.projects[0].source,
            Source::Wrap { name, version } if name == "zlib" && version.is_none()
        ));
        assert!(matches!(
            &cfg.projects[1].source,
            Source::Wrap { name, version } if name == "expat" && version.as_deref() == Some("2.7.1-1")
        ));
        assert_eq!(cfg.projects[1].short_name(), "expat");
    }

    #[test]
    fn local_wrap_directory_is_accepted() {
        let cfg: Config = toml::from_str(
            "third_party_dir = \"tp\"\nwrap_dir = \"subprojects\"\n[[project]]\nwrap = \"thing\"\n",
        )
        .unwrap();
        assert_eq!(cfg.wrap_dir, Some(PathBuf::from("subprojects")));
    }

    #[test]
    fn a_project_cannot_mix_repo_and_wrap() {
        let toml = "third_party_dir = \"tp\"\n\
             [[project]]\n\
             repo = \"https://example.com/x/zlib.git\"\n\
             rev = \"0000000000000000000000000000000000000000\"\n\
             wrap = \"zlib\"\n";
        let err = toml::from_str::<Config>(toml).unwrap_err();
        assert!(format!("{err}").contains("mix"));
    }

    #[test]
    fn a_project_needs_either_repo_or_wrap() {
        let toml = "third_party_dir = \"tp\"\n\
             [[project]]\n\
             rev = \"0000000000000000000000000000000000000000\"\n";
        let err = toml::from_str::<Config>(toml).unwrap_err();
        assert!(format!("{err}").contains("either a git checkout"));
    }

    #[test]
    fn git_projects_accept_cargo_style_branch_tag_and_rev() {
        for (key, expected) in [
            ("branch", GitReference::Branch("main".to_owned())),
            ("tag", GitReference::Tag("v1.2.3".to_owned())),
            ("rev", GitReference::Rev("deadbeef".to_owned())),
        ] {
            let toml = format!(
                "third_party_dir = \"tp\"\n[[project]]\nrepo = \"https://example.com/x.git\"\n{key} = \"{}\"\n",
                expected.value(),
            );
            let cfg: Config = toml::from_str(&toml).unwrap();
            assert!(
                matches!(&cfg.projects[0].source, Source::Git { reference, .. } if reference == &expected)
            );
        }
    }

    #[test]
    fn git_projects_reject_multiple_reference_kinds() {
        let toml = "third_party_dir = \"tp\"\n[[project]]\nrepo = \"https://example.com/x.git\"\nbranch = \"main\"\ntag = \"v1\"\n";
        let err = toml::from_str::<Config>(toml).unwrap_err();
        assert!(format!("{err}").contains("only one of `branch`, `tag`, or `rev`"));
    }
}
