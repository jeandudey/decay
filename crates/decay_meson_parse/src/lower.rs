use {
    crate::node::Node,
    decay_meson_ast::{
        Args,
        AssignStmt,
        BinOp,
        BinOpKind,
        Block,
        Call,
        Dict,
        Expr,
        ForeachStmt,
        IfStmt,
        Index,
        Loc,
        Method,
        Stmt,
        Ternary,
        UnOp,
        UnOpKind, //
    },
    eyre::{
        OptionExt,
        bail, //
    },
    std::str::FromStr,
};

pub fn block(node: &Node) -> eyre::Result<Block> {
    let lines = node
        .as_code_block()
        .ok_or_eyre("expected code block node")?;

    let mut stmts = Vec::new();
    for line in lines {
        match line {
            Node::Assignment { var_name, value } => {
                stmts.push(assign_stmt(var_name, value, false).map(Stmt::Assign)?)
            }
            Node::PlusAssignment { var_name, value } => {
                stmts.push(assign_stmt(var_name, value, true).map(Stmt::Assign)?);
            }
            Node::IfClause { elseblock, ifs } => {
                stmts.push(Stmt::If(IfStmt {
                    arms: ifs
                        .iter()
                        .map(|if_| match if_ {
                            Node::If {
                                block: if_block,
                                condition,
                            } => Ok((expr(condition)?, block(if_block)?)),
                            _ => bail!("expected if node"),
                        })
                        .collect::<eyre::Result<_>>()?,
                    elseblock: match &**elseblock {
                        Node::Empty => None,
                        Node::Else { block: else_block } => block(else_block).map(Some)?,
                        _ => bail!("expected else node"),
                    },
                }));
            }
            Node::ForeachClause {
                varnames,
                items,
                body,
            } => {
                stmts.push(Stmt::Foreach(ForeachStmt {
                    names: varnames
                        .iter()
                        .map(|v| v.expect_id())
                        .collect::<eyre::Result<Vec<_>>>()?,
                    iter: expr(items)?,
                    body: block(body)?,
                }));
            }
            Node::Break => stmts.push(Stmt::Break),
            Node::Continue => stmts.push(Stmt::Continue),
            // Anything else in statement position is an expression evaluated
            // for its effect, which is how most of meson's API is called.
            other => stmts.push(expr(other).map(Stmt::Expr)?),
        }
    }
    Ok(Block(stmts))
}

fn args(node: &Node) -> eyre::Result<Args> {
    let node = node.expect_argument()?;

    Ok(Args {
        pos: node
            .arguments
            .iter()
            .map(expr)
            .collect::<eyre::Result<_>>()?,
        kw: node
            .kwargs
            .iter()
            .map(|pair| Ok((pair.key.expect_id()?, expr(&pair.value)?)))
            .collect::<eyre::Result<_>>()?,
        order: node
            .kwargs
            .iter()
            .map(|pair| pair.key.expect_id())
            .collect::<eyre::Result<_>>()?,
    })
}

fn expr(node: &Node) -> eyre::Result<Expr> {
    match node {
        Node::Id { value } => Ok(Expr::Id(value.clone())),
        Node::String { value, is_fstring } => Ok(if *is_fstring {
            Expr::FormatString(value.clone())
        } else {
            Expr::String(value.clone())
        }),
        Node::Number { value, .. } => Ok(Expr::Int(*value)),
        Node::Boolean { value } => Ok(Expr::Bool(*value)),
        Node::Parenthesized { inner } => expr(inner),
        Node::Array { args } => args
            .expect_argument()?
            .arguments
            .iter()
            .map(expr)
            .collect::<eyre::Result<Vec<_>>>()
            .map(Expr::List),
        Node::Function {
            name,
            args: call_args,
            line,
            col,
        } => Ok(Expr::Call(Call {
            name: name.expect_id()?,
            args: args(call_args)?,
            loc: Loc {
                line: *line,
                col: *col,
            },
        })),
        Node::Method {
            source_object,
            name,
            args: method_args,
            line,
            col,
        } => Ok(Expr::Method(Method {
            obj: expr(source_object).map(Box::new)?,
            name: name.expect_id()?,
            args: args(method_args)?,
            loc: Loc {
                line: *line,
                col: *col,
            },
        })),
        Node::Index { iobject, index } => Ok(Expr::Index(Index {
            obj: expr(iobject).map(Box::new)?,
            index: expr(index).map(Box::new)?,
        })),
        Node::Comparison { ctype, left, right } => Ok(Expr::BinOp(BinOp {
            kind: BinOpKind::from_str(ctype)?,
            lhs: expr(left).map(Box::new)?,
            rhs: expr(right).map(Box::new)?,
        })),
        Node::Arithmetic {
            left,
            right,
            operation,
        } => Ok(Expr::BinOp(BinOp {
            kind: BinOpKind::from_str(operation)?,
            lhs: expr(left).map(Box::new)?,
            rhs: expr(right).map(Box::new)?,
        })),
        Node::Or { left, right } => Ok(Expr::BinOp(BinOp {
            kind: BinOpKind::Or,
            lhs: expr(left).map(Box::new)?,
            rhs: expr(right).map(Box::new)?,
        })),
        Node::And { left, right } => Ok(Expr::BinOp(BinOp {
            kind: BinOpKind::And,
            lhs: expr(left).map(Box::new)?,
            rhs: expr(right).map(Box::new)?,
        })),
        Node::Not { value } => Ok(Expr::UnOp(UnOp {
            kind: UnOpKind::Not,
            val: expr(value).map(Box::new)?,
        })),
        Node::UMinus { value } => Ok(Expr::UnOp(UnOp {
            kind: UnOpKind::Neg,
            val: expr(value).map(Box::new)?,
        })),
        Node::Ternary {
            condition,
            trueblock,
            falseblock,
        } => Ok(Expr::Ternary(Ternary {
            condition: expr(condition).map(Box::new)?,
            trueblock: expr(trueblock).map(Box::new)?,
            falseblock: expr(falseblock).map(Box::new)?,
        })),
        Node::Dict { args } => {
            let kwargs = &args.expect_argument()?.kwargs;
            Ok(Expr::Dict(Dict {
                args: kwargs
                    .iter()
                    .map(|pair| Ok((pair.key.expect_key()?, expr(&pair.value)?)))
                    .collect::<eyre::Result<_>>()?,
                order: kwargs
                    .iter()
                    .map(|pair| pair.key.expect_key())
                    .collect::<eyre::Result<_>>()?,
            }))
        }
        // Meson emits an `EmptyNode` where an expression was expected but the
        // source trailed off — an operator at the end of a logical line
        // outside brackets, say (glib's gnulib probes have one). Meson treats
        // it as falsy; matching that keeps decay's reading of the file the
        // same as meson's own.
        Node::Empty => Ok(Expr::Bool(false)),
        _ => bail!("Unexpected expression node {node:?}"),
    }
}

fn assign_stmt(name: &Node, value: &Node, is_plus: bool) -> eyre::Result<AssignStmt> {
    Ok(AssignStmt {
        name: name.expect_id()?,
        val: expr(value)?,
        is_plus,
    })
}
