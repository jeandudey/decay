mod arena;
mod logic;
pub mod stats;
mod var;

pub use {
    arena::{
        ANY_OTHER,
        Arena,
        Pc,
        Var,
        VarId,
        VarKind, //
    },
    logic::Logic,
    var::{
        Variant,
        Variational, //
    },
};
