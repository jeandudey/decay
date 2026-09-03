use serde::Deserialize;

/// One option as `mesonbuild.optinterpreter` understood it.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum OptionNode {
    #[serde(rename = "UserBooleanOption")]
    Bool {
        name: String,
        value: bool,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        deprecated: bool,
    },
    #[serde(rename = "UserComboOption")]
    Combo {
        name: String,
        value: String,
        choices: Vec<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        deprecated: bool,
    },
    #[serde(rename = "UserStringOption")]
    Str {
        name: String,
        value: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        deprecated: bool,
    },
    #[serde(rename = "UserIntegerOption")]
    Integer {
        name: String,
        value: i64,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        deprecated: bool,
    },
    #[serde(rename = "UserStringArrayOption", alias = "UserArrayOption")]
    Array {
        name: String,
        #[serde(default)]
        value: Vec<String>,
        #[serde(default)]
        choices: Option<Vec<String>>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        deprecated: bool,
    },
    #[serde(rename = "UserFeatureOption")]
    Feature {
        name: String,
        value: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        deprecated: bool,
    },
}
