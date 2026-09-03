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
        collections::{BTreeMap, HashMap},
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
    pub third_party_dir: PathBuf,
    pub systems: HashMap<String, String>,
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
    #[serde(default)]
    pub options: BTreeMap<String, OptionValue>,
    #[serde(default)]
    pub host_machine: Machine,
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
            name.push_str(&segment);
        }
        let hash = Sha256::digest(self.0.to_string());
        Ok(format!("{name}-{}", hex::encode(&hash[..8])))
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
}
