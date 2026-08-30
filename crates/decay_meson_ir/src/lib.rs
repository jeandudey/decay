use {
    decay_meson_ast as ast,
    std::{
        fmt::{
            self,
            Display, //
        }, //
    },
};

mod lower;

pub fn normalize(block: &ast::Block) -> Vec<Stmt> {
    let mut ctx = lower::Lower::new();
    ctx.block(&block.0)
}

#[derive(Debug)]
pub enum Expr {
    Atom(Atom),
    Call(RValue),
}

#[derive(Debug)]
pub enum Stmt {
    Bind(BindStmt),
    If(IfStmt),
}

#[derive(Debug)]
pub enum Atom {
    Var(String),
    String(String),
    Int(i64),
    Bool(bool),
}

#[derive(Debug)]
pub enum RValue {
    Pure(Atom),
    Array(Vec<Atom>),
    Call(Call),
    Method(Method),
    Index(Index),
    BinOp(BinOp),
    Not(Atom),
}

#[derive(Debug)]
pub struct Call {
    pub name: String,
    pub args: Args,
}

#[derive(Debug)]
pub struct Method {
    pub obj: Atom,
    pub name: String,
    pub args: Args,
}

#[derive(Debug)]
pub struct Args {
    pub pos: Vec<Atom>,
    pub kw: Vec<(String, Atom)>,
}

#[derive(Debug)]
pub struct Index {
    pub obj: Atom,
    pub index: Atom,
}

#[derive(Debug)]
pub struct BinOp {
    pub kind: BinOpKind,
    pub lhs: Atom,
    pub rhs: Atom,
}

#[derive(Debug)]
pub enum BinOpKind {
    Add,
    And,
    Eq,
    Ne,
    Or,
}

impl From<ast::BinOpKind> for BinOpKind {
    fn from(value: ast::BinOpKind) -> Self {
        match value {
            ast::BinOpKind::Add => BinOpKind::Add,
            ast::BinOpKind::And => BinOpKind::And,
            ast::BinOpKind::Eq => BinOpKind::Eq,
            ast::BinOpKind::Ne => BinOpKind::Ne,
            ast::BinOpKind::Or => BinOpKind::Or,
        }
    }
}

#[derive(Debug)]
pub struct BindStmt {
    pub name: String,
    pub rvalue: RValue,
}

#[derive(Debug)]
pub struct IfStmt {
    pub cond: Atom,
    pub then_block: Vec<Stmt>,
    pub else_block: Vec<Stmt>,
}

#[derive(Debug)]
pub struct IfClause {
    pub cond: Atom,
    pub block: Vec<Stmt>,
}

impl Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stmt::Bind(v) => {
                write!(f, "let {} = {}", v.name, v.rvalue)
            }
            Stmt::If(v) => {
                write!(
                    f,
                    "if {} then\n{{\n{}\n}}",
                    v.cond,
                    v.then_block
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                )?;
                if !v.else_block.is_empty() {
                    write!(
                        f,
                        "\nelse\n{{\n{}\n}}",
                        v.else_block
                            .iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join("\n")
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Atom::Var(v) => write!(f, "{v}"),
            Atom::String(v) => write!(f, "{v:?}"),
            Atom::Int(v) => write!(f, "{v}"),
            Atom::Bool(v) => write!(f, "{v}"),
        }
    }
}

impl Display for RValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RValue::Pure(v) => write!(f, "pure {v}"),
            RValue::Call(v) => write!(f, "call {} {}", v.name, v.args),
            RValue::Method(v) => write!(f, "method {}.{}{}", v.obj.to_string(), v.name, v.args),
            RValue::Index(v) => write!(f, "index {}[{}]", v.obj.to_string(), v.index.to_string()),
            RValue::Array(v) => write!(
                f,
                "[{}]",
                v.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            RValue::BinOp(v) => {
                let op = match v.kind {
                    BinOpKind::Add => "+",
                    BinOpKind::And => "and",
                    BinOpKind::Eq => "==",
                    BinOpKind::Ne => "!=",
                    BinOpKind::Or => "or",
                };
                write!(f, "{} {op} {}", v.lhs, v.rhs)
            }
            RValue::Not(v) => write!(f, "not {v}"),
        }
    }
}

impl Display for Args {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pos = self
            .pos
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ");

        let kw = self
            .kw
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ");

        match (pos.is_empty(), kw.is_empty()) {
            (true, true) => write!(f, "()"),
            (true, false) => write!(f, "({kw})"),
            (false, true) => write!(f, "({pos})"),
            (false, false) => write!(f, "({pos}, {kw})"),
        }
    }
}
