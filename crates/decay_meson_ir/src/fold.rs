use {
    crate::{Args, Atom, BinOp, BinOpKind, BindStmt, Call, Index, Method, RValue, Stmt},
    std::collections::HashMap,
};

pub(crate) fn fold_block(block: &mut Vec<Stmt>, env: &mut HashMap<String, Atom>) {
    let mut out = Vec::new();
    for stmt in block.drain(..) {
        match stmt {
            Stmt::Bind(BindStmt { name, rvalue }) => {
                let rvalue = resolve(rvalue, env);
                match eval_const(&rvalue) {
                    Some(v) => {
                        env.insert(name.clone(), v.clone());
                        out.push(Stmt::Bind(BindStmt {
                            name,
                            rvalue: RValue::Pure(v),
                        }));
                    }
                    None => {
                        env.remove(&name);
                        out.push(Stmt::Bind(BindStmt { name, rvalue }));
                    }
                }
            }
            stmt => out.push(stmt),
        }
    }
    *block = out;
}

fn resolve(rvalue: RValue, env: &HashMap<String, Atom>) -> RValue {
    match rvalue {
        RValue::Pure(v) => RValue::Pure(subst(v, env)),
        RValue::Array(v) => RValue::Array(v.into_iter().map(|v| subst(v, env)).collect()),
        RValue::Call(Call {
            name,
            args: Args { pos, kw },
        }) => RValue::Call(Call {
            name,
            args: Args {
                pos: pos.into_iter().map(|v| subst(v, env)).collect(),
                kw: kw.into_iter().map(|(k, v)| (k, subst(v, env))).collect(),
            },
        }),
        RValue::Method(Method {
            obj,
            name,
            args: Args { pos, kw },
        }) => RValue::Method(Method {
            obj: subst(obj, env),
            name,
            args: Args {
                pos: pos.into_iter().map(|v| subst(v, env)).collect(),
                kw: kw.into_iter().map(|(k, v)| (k, subst(v, env))).collect(),
            },
        }),
        RValue::Index(Index { obj, index }) => RValue::Index(Index {
            obj: subst(obj, env),
            index: subst(index, env),
        }),
        RValue::BinOp(BinOp { kind, lhs, rhs }) => RValue::BinOp(BinOp {
            kind,
            lhs: subst(lhs, env),
            rhs: subst(rhs, env),
        }),
        RValue::Not(v) => RValue::Not(subst(v, env)),
    }
}

fn subst(atom: Atom, env: &HashMap<String, Atom>) -> Atom {
    match atom {
        Atom::Var(ref v) => env.get(v).cloned().unwrap_or(atom),
        lit => lit,
    }
}

fn eval_const(rvalue: &RValue) -> Option<Atom> {
    match rvalue {
        RValue::Pure(v) if v.is_lit() => Some(v.clone()),
        RValue::Not(Atom::Bool(v)) => Some(Atom::Bool(!v)),
        RValue::BinOp(BinOp {
            kind: BinOpKind::Add,
            lhs: Atom::Int(lhs),
            rhs: Atom::Int(rhs),
        }) => Some(Atom::Int(lhs + rhs)),
        RValue::BinOp(BinOp {
            kind: BinOpKind::Add,
            lhs: Atom::String(lhs),
            rhs: Atom::String(rhs),
        }) => Some(Atom::String(format!("{lhs}{rhs}"))),
        _ => None,
    }
}
