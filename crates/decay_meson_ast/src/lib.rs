use {
    eyre::bail,
    std::{
        collections::HashMap,
        fmt::{
            self,
            Display, //
        },
        str::FromStr,
    },
};

/// Where a node came from, for diagnostics.
///
/// Meson build definitions are large and full of functions this executor does
/// not know yet; without a file and line an "unimplemented" error is close to
/// useless on a real project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Loc {
    pub line: u32,
    pub col: u32,
}

impl Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[derive(Debug)]
pub struct Block(pub Vec<Stmt>);

#[derive(Debug)]
pub enum Stmt {
    Expr(Expr),
    Assign(AssignStmt),
    If(IfStmt),
    Foreach(ForeachStmt),
    Break,
    Continue,
}

#[derive(Debug)]
pub enum Expr {
    Id(String),
    /// A plain string literal.
    String(String),
    /// An `f'...'` string, whose `@name@` holes name variables in scope.
    FormatString(String),
    List(Vec<Expr>),
    Dict(Dict),
    Int(i64),
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
    pub loc: Loc,
}

#[derive(Debug)]
pub struct Method {
    pub obj: Box<Expr>,
    pub name: String,
    pub args: Args,
    pub loc: Loc,
}

#[derive(Debug)]
pub struct Args {
    pub pos: Vec<Expr>,
    pub kw: HashMap<String, Expr>,
    /// Keyword names in source order, so evaluation order is reproducible.
    pub order: Vec<String>,
}

impl Args {
    pub fn empty() -> Self {
        Self {
            pos: Vec::new(),
            kw: HashMap::new(),
            order: Vec::new(),
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOpKind {
    Not,
    Neg,
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
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
}

impl BinOpKind {
    /// Whether the operator yields a boolean, and so can be turned straight
    /// into a presence condition.
    pub fn is_predicate(self) -> bool {
        matches!(
            self,
            Self::And
                | Self::Or
                | Self::Eq
                | Self::Ne
                | Self::Lt
                | Self::Le
                | Self::Gt
                | Self::Ge
                | Self::In
                | Self::NotIn
        )
    }
}

impl Display for BinOpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::And => "and",
            Self::Or => "or",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::In => "in",
            Self::NotIn => "not in",
        };
        f.write_str(s)
    }
}

impl FromStr for BinOpKind {
    type Err = eyre::Report;

    fn from_str(s: &str) -> eyre::Result<Self> {
        Ok(match s {
            "==" => Self::Eq,
            "!=" => Self::Ne,
            "<" => Self::Lt,
            "<=" => Self::Le,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            "in" => Self::In,
            "notin" | "not in" => Self::NotIn,
            "add" => Self::Add,
            "sub" => Self::Sub,
            "mul" => Self::Mul,
            "div" => Self::Div,
            "mod" => Self::Mod,
            _ => bail!("unknown binary operator `{s}`"),
        })
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
    pub val: Expr,
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

#[derive(Debug, Clone)]
pub struct ProjectOption {
    pub description: Option<String>,
    pub kind: ProjectOptionKind,
    pub deprecated: bool,
}

#[derive(Debug, Clone)]
pub enum ProjectOptionKind {
    Bool { value: bool },
    Combo { choices: Vec<String>, value: String },
    /// A free-form option: no finite domain, so it can only ever be executed
    /// with whatever value the importer was told to use.
    String { value: String },
    Integer { value: i64 },
    Array { choices: Vec<String>, value: Vec<String> },
    /// `auto` / `enabled` / `disabled`.
    Feature { value: String },
}

impl ProjectOptionKind {
    /// The domain to branch over, and the index of the default value, when the
    /// option is left dynamic.
    pub fn domain(&self) -> Option<(Vec<String>, usize)> {
        match self {
            Self::Bool { value } => {
                let choices = vec!["true".to_owned(), "false".to_owned()];
                Some((choices, usize::from(!*value)))
            }
            Self::Combo { choices, value } => {
                let default = choices.iter().position(|c| c == value).unwrap_or(0);
                Some((choices.clone(), default))
            }
            Self::Feature { value } => {
                let choices = ["enabled", "disabled", "auto"].map(str::to_owned).to_vec();
                let default = choices.iter().position(|c| c == value).unwrap_or(2);
                Some((choices, default))
            }
            Self::String { .. } | Self::Integer { .. } | Self::Array { .. } => None,
        }
    }
}
