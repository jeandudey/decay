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
        fs,
        path::{
            Path,
            PathBuf, //
        },
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
    pub dependencies: BTreeMap<String, String>,
    #[serde(rename = "project")]
    pub projects: Vec<Project>,
}

impl Config {
    pub fn from_file(path: impl AsRef<Path>) -> eyre::Result<Self> {
        let file = fs::read_to_string(path).wrap_err("Failed to load configuration file")?;
        toml::from_str(&file).wrap_err("Failed to parse configuration file")
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
