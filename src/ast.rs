use {
    crate::ast::interp::Interp,
    std::{
        collections::HashMap,
        path::Path, //
    },
};

mod interp;
mod sym;

pub fn eval(root: impl AsRef<Path>, systems: &HashMap<String, String>) -> eyre::Result<()> {
    let mut interp = Interp::new(systems);
    interp.run(root)?;
    todo!("{interp:?}")
}
