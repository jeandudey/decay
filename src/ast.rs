use {
    crate::ast::interp::Interp,
    decay_meson_ast::Block,
    eyre::Context,
    std::{
        collections::HashMap,
        path::Path, //
    },
};

mod interp;
mod lower;
mod raw;
mod sym;

pub fn parse(path: impl AsRef<Path>) -> eyre::Result<Block> {
    let node = raw::parse(path)?;
    let block = lower::block(&node).wrap_err("Failed to lower AST")?;
    Ok(block)
}

pub fn eval(root: impl AsRef<Path>, systems: &HashMap<String, String>) -> eyre::Result<()> {
    let mut interp = Interp::new(systems);
    interp.run(root)?;
    todo!("{interp:?}")
}
