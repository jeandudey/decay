use {
    decay_meson_ast as ast,
    std::{
        collections::HashMap,
        fmt::{
            self,
            Display, //
        }, //
    },
};

struct Lower {
    out: Vec<Stmt>,
    vars: HashMap<String, String>,
    tmp: u32,
}

impl Lower {
    fn block(&mut self, block: &[ast::Stmt]) -> Vec<Stmt> {
        self.sub(|s| {
            for stmt in block.iter() {
                s.stmt(stmt);
            }
        })
    }

    fn sub(&mut self, f: impl FnOnce(&mut Self)) -> Vec<Stmt> {
        let saved = std::mem::take(&mut self.out);
        f(self);
        std::mem::replace(&mut self.out, saved)
    }

    fn stmt(&mut self, stmt: &ast::Stmt) {
        match stmt {
            ast::Stmt::Expr(v) => {
                self.atom(v);
            }
            ast::Stmt::Assign(v) => {
                let val = self.atom(&v.val);
                if v.is_plus {
                    let old = self.vars.get(&v.name).unwrap();
                    let new = self.bind(RValue::BinOp(BinOp {
                        kind: BinOpKind::Add,
                        lhs: Atom::Var(old.clone()),
                        rhs: val,
                    }));
                    self.vars.insert(v.name.clone(), new);
                } else {
                    let tmp = self.bind(RValue::Pure(val));
                    self.vars.insert(v.name.clone(), tmp);
                }
            }
            ast::Stmt::If(v) => {
                self.if_chain(
                    v.arms.as_slice(),
                    v.elseblock.as_ref().map(|v| v.0.as_slice()).unwrap_or(&[]),
                );
            }
            _ => todo!("{stmt:?}"),
        }
    }

    fn if_chain(&mut self, clauses: &[(ast::Expr, ast::Block)], else_block: &[ast::Stmt]) {
        let (head, rest) = clauses.split_first().unwrap();
        let cond = self.atom(&head.0);
        let then_block = self.block(&head.1.0);
        let else_block = if rest.is_empty() {
            self.block(else_block)
        } else {
            self.sub(|s| s.if_chain(rest, else_block))
        };
        self.out.push(Stmt::If(IfStmt {
            cond,
            then_block,
            else_block,
        }));
    }

    fn atom(&mut self, expr: &ast::Expr) -> Atom {
        match expr {
            ast::Expr::Id(v) => {
                if let Some(tmp) = self.vars.get(v) {
                    Atom::Var(tmp.clone())
                } else {
                    Atom::Var(v.clone())
                }
            }
            ast::Expr::String(v) => Atom::String(v.clone()),
            // TODO: Parser always puts is_fstring, why?
            ast::Expr::FormatString(v) => Atom::String(v.clone()),
            ast::Expr::Int(v) => Atom::Int(*v),
            ast::Expr::Bool(v) => Atom::Bool(*v),
            ast::Expr::Array(v) => {
                let items = v.iter().map(|v| self.atom(v)).collect();
                let tmp = self.bind(RValue::Array(items));
                Atom::Var(tmp)
            }
            ast::Expr::Call(v) => {
                let call = Call {
                    name: v.name.clone(),
                    args: self.args(&v.args),
                };
                let tmp = self.bind(RValue::Call(call));
                Atom::Var(tmp)
            }
            ast::Expr::Method(v) => {
                let method = Method {
                    obj: self.atom(&v.obj),
                    name: v.name.clone(),
                    args: self.args(&v.args),
                };
                let tmp = self.bind(RValue::Method(method));
                Atom::Var(tmp)
            }
            ast::Expr::Index(v) => {
                let index = Index {
                    obj: self.atom(&v.obj),
                    index: self.atom(&v.index),
                };
                let tmp = self.bind(RValue::Index(index));
                Atom::Var(tmp)
            }
            ast::Expr::BinOp(v) => {
                let binop = BinOp {
                    kind: BinOpKind::from(v.kind),
                    lhs: self.atom(&v.lhs),
                    rhs: self.atom(&v.rhs),
                };
                let tmp = self.bind(RValue::BinOp(binop));
                Atom::Var(tmp)
            }
            ast::Expr::UnOp(ast::UnOp {
                kind: ast::UnOpKind::Not,
                val,
            }) => {
                let val = self.atom(&val);
                let tmp = self.bind(RValue::Not(val));
                Atom::Var(tmp)
            }
            _ => todo!("{expr:?}"),
        }
    }

    fn args(&mut self, args: &ast::Args) -> Args {
        let pos = args.positional.iter().map(|v| self.atom(v)).collect();

        let kw = args
            .order
            .iter()
            .map(|k| (k.clone(), self.atom(args.kwargs.get(k).unwrap())))
            .collect();

        Args { pos, kw }
    }

    fn bind(&mut self, rvalue: RValue) -> String {
        let tmp = self.tmp();
        self.out.push(Stmt::Bind(BindStmt {
            name: tmp.clone(),
            rvalue,
        }));
        tmp
    }

    fn tmp(&mut self) -> String {
        self.tmp += 1;
        format!("%{}", self.tmp - 1)
    }
}

pub fn normalize(block: &ast::Block) -> Vec<Stmt> {
    let mut ctx = Lower {
        out: Vec::new(),
        vars: HashMap::new(),
        tmp: 0,
    };
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
            (false, false) => write!(f, "({kw}, {pos})"),
        }
    }
}
