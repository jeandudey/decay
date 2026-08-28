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
}

#[derive(Debug)]
pub enum Expr {
    Id(String),
    String(String),
    Array(Vec<Expr>),
    Number(i64),
    Bool(bool),
    Call(Call),
    Method(Method),
    Index(Index),
    UnOp(UnOp),
    BinOp(BinOp),
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

#[derive(Debug)]
pub enum BinOpKind {
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
pub struct AssignStmt {
    pub name: String,
    pub value: Expr,
    pub is_plus: bool,
}

#[derive(Debug)]
pub struct IfStmt {
    pub arms: Vec<(Expr, Block)>,
    pub else_: Option<Block>,
}

#[derive(Debug)]
pub struct MesonProject {
    pub name: String,
}

pub fn eval(block: &Block) -> eyre::Result<MesonProject> {
    let mut interp = Interp::new();
    interp.exec_block(block)?;
    //let mut meson_project = None;
    //for statement in &ast.0 {
    //    match statement {
    //        Stmt::(function) => match function.name.as_str() {
    //            "project" => {
    //                let mut args = function.args.positional.iter();
    //                let name = args.next().ok_or_eyre("Missing project name")?;
    //            }
    //            "subdir" => {
    //                let dir = function
    //                    .args
    //                    .positional
    //                    .get(0)
    //                    .ok_or_eyre("Expected directory name")?
    //                    .as_string()?;
    //                println!("TODO GO INTO SUBDIR {dir:?}");
    //            }
    //            _ => todo!("{function:?}"),
    //        },
    //        _ => bail!("Unhandled statement {statement:?}"),
    //    }
    //}
    //meson_project.ok_or_eyre("Meson project not initialized")
    todo!("{block:?}")
}
