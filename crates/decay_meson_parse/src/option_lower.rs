use {
    crate::option_node::OptionNode,
    decay_meson_ast::{
        ProjectOption,
        ProjectOptionKind,
        ProjectOptions, //
    }, //
};

pub fn options(options: &[OptionNode]) -> ProjectOptions {
    options
        .iter()
        .map(|v| match v {
            OptionNode::Bool {
                name,
                value,
                description,
                deprecated,
            } => opt(name, description, *deprecated, ProjectOptionKind::Bool {
                value: *value,
            }),
            OptionNode::Combo {
                name,
                value,
                choices,
                description,
                deprecated,
            } => opt(name, description, *deprecated, ProjectOptionKind::Combo {
                choices: choices.clone(),
                value: value.clone(),
            }),
            OptionNode::Str {
                name,
                value,
                description,
                deprecated,
            } => opt(name, description, *deprecated, ProjectOptionKind::String {
                value: value.clone(),
            }),
            OptionNode::Integer {
                name,
                value,
                description,
                deprecated,
            } => opt(name, description, *deprecated, ProjectOptionKind::Integer {
                value: *value,
            }),
            OptionNode::Array {
                name,
                value,
                choices,
                description,
                deprecated,
            } => opt(name, description, *deprecated, ProjectOptionKind::Array {
                choices: choices.clone().unwrap_or_default(),
                value: value.clone(),
            }),
            OptionNode::Feature {
                name,
                value,
                description,
                deprecated,
            } => opt(name, description, *deprecated, ProjectOptionKind::Feature {
                value: value.clone(),
            }),
        })
        .collect()
}

fn opt(
    name: &str,
    description: &Option<String>,
    deprecated: bool,
    kind: ProjectOptionKind,
) -> (String, ProjectOption) {
    (name.to_owned(), ProjectOption {
        description: description.clone(),
        kind,
        deprecated,
    })
}
