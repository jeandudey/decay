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
        fmt,
        fs,
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
    #[serde(rename = "project")]
    pub projects: Vec<Project>,
}

impl Config {
    pub fn from_file(path: impl AsRef<Path>) -> eyre::Result<Self> {
        let file = fs::read_to_string(path).wrap_err("Failed to load configuration file")?;
        let config: Self =
            toml::from_str(&file).wrap_err("Failed to parse configuration file")?;
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> eyre::Result<()> {
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
    /// what it can tell apart is exactly what it was told about.
    pub fn constraint_domain(&self, setting: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for value in self.probes.values().flat_map(ProbeValue::values) {
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

/// One value of one constraint, written the way a `select()` key is:
/// `prelude//abi/constraints:abi[gnu]`.
#[derive(Debug, Clone)]
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

#[derive(Debug, Deserialize)]
pub struct Project {
    pub repo: Repo,
    pub rev: String,
    /// Options pinned to a fixed value.
    ///
    /// Anything left out stays a build-time choice, which is the point: most
    /// projects should pin nothing here.
    #[serde(default)]
    pub options: BTreeMap<String, OptionValue>,
    #[serde(default)]
    pub host_machine: Machine,
    #[serde(default)]
    pub build_machine: Machine,
    /// Other projects in this file that must be imported before this one,
    /// named the way `short_name()` spells them (`glib`, `graphene`).
    ///
    /// A `dependency()` here resolves against a sibling only once that sibling
    /// has been executed, so a consumer has to name every provider it looks
    /// up. Projects whose `depends` are all already imported run in parallel;
    /// left empty everywhere, projects run in file order, one at a time, as
    /// they always have.
    #[serde(default)]
    pub depends: Vec<String>,
}

impl Project {
    pub fn is_full_sha(&self) -> bool {
        self.rev.len() == 40 && self.rev.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

#[derive(Debug, Deserialize)]
pub struct Repo(pub Url);

impl Repo {
    pub fn ident(&self) -> eyre::Result<String> {
        let mut name = String::new();
        let segments = self.0
            .path_segments()
            .ok_or(eyre!("Repository URL doesn't contain segments, this probably shouldn't be an error if someone hosts a git repository at the URL root"))?;
        for segment in segments {
            name.push_str(segment);
        }
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    String(String),
    List(Vec<String>),
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
