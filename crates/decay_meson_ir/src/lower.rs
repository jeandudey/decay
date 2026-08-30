use {
    crate::{
        Args,
        Atom,
        BinOp,
        BinOpKind,
        BindStmt,
        Call,
        IfStmt,
        Index,
        Method,
        RValue,
        Stmt, //
    },
    decay_meson_ast as ast,
    std::collections::HashMap, //
};

pub(crate) struct Lower {
    out: Vec<Stmt>,
    vars: HashMap<String, String>,
    tmp: u32,
}

impl Lower {
    pub(crate) fn new() -> Self {
        Self {
            out: Vec::new(),
            vars: HashMap::new(),
            tmp: 0,
        }
    }
    pub(crate) fn block(&mut self, block: &[ast::Stmt]) -> Vec<Stmt> {
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
