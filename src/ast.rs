use {
    crate::ast::interp::Interp,
    eyre::{
        Context,
        bail, //
    },
    std::{
        collections::HashMap,
        path::Path,
        str::FromStr, //
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

#[derive(Debug)]
pub struct Block(pub Vec<Stmt>);

#[derive(Debug)]
pub enum Stmt {
    Expr(Expr),
    Assign(AssignStmt),
    If(IfStmt),
    Foreach(ForeachStmt),
}

#[derive(Debug)]
pub enum Expr {
    Id(String),
    String(String),
    Array(Vec<Expr>),
    Dict(Dict),
    Number(i64),
    Bool(bool),
    Call(Call),
    Method(Method),
    Index(Index),
    UnOp(UnOp),
    BinOp(BinOp),
    Ternary(Ternary),
}

impl Expr {
    pub fn as_string(&self) -> eyre::Result<&str> {
        match self {
            Self::String(v) => Ok(v),
            _ => bail!("expected string value"),
        }
    }
}

#[derive(Debug)]
pub struct Dict {
    pub args: HashMap<String, Expr>,
    pub order: Vec<String>,
}

#[derive(Debug)]
pub struct Call {
    pub name: String,
    pub args: Args,
}

#[derive(Debug)]
pub struct Method {
    pub obj: Box<Expr>,
    pub name: String,
    pub args: Args,
}

#[derive(Debug)]
pub struct Args {
    pub positional: Vec<Expr>,
    pub kwargs: HashMap<String, Expr>,
    pub order: Vec<String>,
}

#[derive(Debug)]
pub struct Index {
    pub obj: Box<Expr>,
    pub index: Box<Expr>,
}

#[derive(Debug)]
pub struct UnOp {
    pub kind: UnOpKind,
    pub val: Box<Expr>,
}

#[derive(Debug)]
pub enum UnOpKind {
    Not,
}

#[derive(Debug)]
pub struct BinOp {
    pub kind: BinOpKind,
    pub lhs: Box<Expr>,
    pub rhs: Box<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOpKind {
    Add,
    And,
    Eq,
    Ne,
    Or,
}

impl FromStr for BinOpKind {
    type Err = eyre::Report;

    fn from_str(s: &str) -> eyre::Result<Self> {
        match s {
            "==" => Ok(BinOpKind::Eq),
            "!=" => Ok(BinOpKind::Ne),
            _ => bail!("unknown binary operator type {s}"),
        }
    }
}

#[derive(Debug)]
pub struct Ternary {
    pub condition: Box<Expr>,
    pub trueblock: Box<Expr>,
    pub falseblock: Box<Expr>,
}

#[derive(Debug)]
pub struct AssignStmt {
    pub name: String,
    pub value: Expr,
    pub is_plus: bool,
}

#[derive(Debug)]
pub struct IfStmt {
    pub arms: Vec<(Expr, Block)>,
    pub elseblock: Option<Block>,
}

#[derive(Debug)]
pub struct ForeachStmt {
    pub names: Vec<String>,
    pub iter: Expr,
    pub body: Block,
}

pub type ProjectOptions = HashMap<String, ProjectOption>;

#[derive(Debug)]
pub struct ProjectOption {
    pub description: Option<String>,
    pub kind: ProjectOptionKind,
    pub deprecated: bool,
}

#[derive(Debug)]
pub enum ProjectOptionKind {
    Bool { value: bool },
    Combo { choices: Vec<String>, value: String },
}

#[derive(Debug)]
pub struct MesonProject {
    pub name: String,
}

pub fn eval(
    root: impl AsRef<Path>,
    systems: &HashMap<String, String>,
) -> eyre::Result<MesonProject> {
    let mut interp = Interp::new(systems);
    interp.run(root)?;
    todo!("{interp:?}")
}
