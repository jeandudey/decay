use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum OptionNode {
    #[serde(rename = "UserBooleanOption")]
    Bool {
        name: String,
        value: bool,
        #[serde(default)]
        description: Option<String>,
        deprecated: bool,
    },
    #[serde(rename = "UserComboOption")]
    Combo {
        name: String,
        value: String,
        choices: Vec<String>,
        description: Option<String>,
    },
}
