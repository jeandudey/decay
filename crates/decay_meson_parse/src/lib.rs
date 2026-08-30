use {
    crate::option_node::OptionNode,
    decay_meson_ast::{
        Block,
        ProjectOptions, //
    },
    eyre::Context,
    std::path::Path,
    tracing::instrument,
};

mod lower;
mod node;
mod option_lower;
mod option_node;
mod py;

#[instrument(level = "trace", err)]
pub fn parse_build(path: &Path) -> eyre::Result<Block> {
    let json = py::parse_build(path)
        .wrap_err_with(|| path.display().to_string())
        .wrap_err("Failed to parse meson build file")?;
    let node = serde_json::from_str(&json).wrap_err("Failed to deserialize AST")?;
    lower::block(&node)
}

#[instrument(level = "trace", err)]
pub fn parse_options(path: &Path) -> eyre::Result<ProjectOptions> {
    let json = py::parse_options(&path)
        .wrap_err_with(|| path.display().to_string())
        .wrap_err("Failed to parse meson options file")?;
    let nodes: Vec<OptionNode> =
        serde_json::from_str(&json).wrap_err("Failed to deserialize options")?;
    Ok(option_lower::options(&nodes))
}
