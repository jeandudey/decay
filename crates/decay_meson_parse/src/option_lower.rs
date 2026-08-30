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
            } => (
                name.clone(),
                ProjectOption {
                    description: description.clone(),
                    kind: ProjectOptionKind::Bool { value: *value },
                    deprecated: *deprecated,
                },
            ),
            OptionNode::Combo {
                name,
                value,
                choices,
                description,
                deprecated,
            } => (
                name.clone(),
                ProjectOption {
                    description: description.clone(),
                    kind: ProjectOptionKind::Combo {
                        choices: choices.clone(),
                        value: value.clone(),
                    },
                    deprecated: *deprecated,
                },
            ),
        })
        .collect()
}
