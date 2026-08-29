use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum MesonOption {
    #[serde(rename = "UserBooleanOption")]
    Bool {
        value: bool,
        #[serde(default)]
        description: Option<String>,
        deprecated: bool,
    },
    #[serde(rename = "UserComboOption")]
    Combo {
        value: String,
        choices: Vec<String>,
        description: Option<String>,
    },
}
