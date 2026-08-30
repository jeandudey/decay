use {
    crate::ast::{
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
        Method,
        ProjectOption,
        ProjectOptionKind,
        ProjectOptions,
        Stmt,
        Ternary,
        UnOp,
        UnOpKind,
        raw::{
            Node,
            OptionNode, //
        }, //
    },
    eyre::{
        OptionExt,
        bail, //
    },
    std::str::FromStr,
    tracing::warn,
};

pub fn block(node: &Node) -> eyre::Result<Block> {
    let lines = node
        .as_code_block()
        .ok_or_eyre("expected code block node")?;

    let mut stmts = Vec::new();
    for line in lines {
        match line {
            Node::Function { .. } | Node::Method { .. } => {
                stmts.push(expr(&line).map(Stmt::Expr)?);
            }
            Node::Assignment {
                var_name, value, ..
            } => stmts.push(assign_stmt(&var_name, &value, false).map(Stmt::Assign)?),
            Node::IfClause { elseblock, ifs } => {
                stmts.push(Stmt::If(IfStmt {
                    arms: ifs
                        .iter()
                        .map(|if_| match if_ {
                            Node::If {
                                block: if_block,
                                condition,
                            } => Ok((expr(&condition)?, block(&if_block)?)),
                            _ => bail!("expected if node"),
                        })
                        .collect::<eyre::Result<_>>()?,
                    elseblock: match &**elseblock {
                        Node::Empty => None,
                        Node::Else { block: else_block } => block(&else_block).map(Some)?,
                        _ => bail!("expected else node"),
                    },
                }));
            }
            Node::PlusAssignment {
                var_name,
                value,
                operator,
            } => {
                if let Some(op) = operator {
                    warn!(?op, "unhandled operator");
                }

                stmts.push(assign_stmt(var_name, value, true).map(Stmt::Assign)?);
            }
            Node::ForeachClause {
                varnames,
                items,
                body,
            } => {
                stmts.push(Stmt::Foreach(ForeachStmt {
                    varnames: varnames
                        .iter()
                        .map(|v| id(v))
                        .collect::<eyre::Result<Vec<_>>>()?,
                    items: expr(items)?,
                    body: block(&body)?,
                }));
            }
            _ => bail!("Unexpected statement {line:?}"),
        }
    }
    Ok(Block(stmts))
}

fn lower_call(name: &Node, call_args: &Node) -> eyre::Result<Call> {
    Ok(Call {
        name: id(name)?,
        args: args(call_args)?,
    })
}

fn args(node: &Node) -> eyre::Result<Args> {
    let node = node.as_argument().ok_or_eyre("expected argument node")?;

    Ok(Args {
        positional: node
            .arguments
            .iter()
            .map(expr)
            .collect::<eyre::Result<_>>()?,
        kwargs: node
            .kwargs
            .iter()
            .map(|pair| Ok((id(&pair.key)?, expr(&pair.value)?)))
            .collect::<eyre::Result<_>>()?,
        order: node
            .kwargs
            .iter()
            .map(|pair| id(&pair.key))
            .collect::<eyre::Result<_>>()?,
    })
}

fn expr(node: &Node) -> eyre::Result<Expr> {
    match node {
        Node::Id { value } => Ok(Expr::Id(value.clone())),
        Node::String { value, .. } => Ok(Expr::String(value.clone())),
        Node::Number { value, .. } => Ok(Expr::Number(*value)),
        Node::Boolean { value } => Ok(Expr::Bool(*value)),
        Node::Array { args } => args
            .as_argument()
            .ok_or_eyre("expected argument node")?
            .arguments
            .iter()
            .map(expr)
            .collect::<eyre::Result<Vec<_>>>()
            .map(Expr::Array),
        Node::Function { name, args } => lower_call(name, args).map(Expr::Call),
        Node::Method {
            source_object,
            name,
            args,
        } => method(source_object, name, args).map(Expr::Method),
        Node::Index { iobject, index } => Ok(Expr::Index(Index {
            obj: expr(iobject).map(Box::new)?,
            index: expr(index).map(Box::new)?,
        })),
        Node::Comparison {
            ctype,
            left,
            operator,
            right,
        } => {
            if let Some(op) = operator {
                warn!(?op, "unhandled operator");
            }

            Ok(Expr::BinOp(BinOp {
                kind: BinOpKind::from_str(ctype)?,
                lhs: expr(left).map(Box::new)?,
                rhs: expr(right).map(Box::new)?,
            }))
        }
        Node::Or {
            left,
            operator,
            right,
        } => {
            if let Some(op) = operator {
                warn!(?op, "unhandled operator");
            }

            Ok(Expr::BinOp(BinOp {
                kind: BinOpKind::Or,
                lhs: expr(left).map(Box::new)?,
                rhs: expr(right).map(Box::new)?,
            }))
        }
        Node::Not { operator, value } => {
            if let Some(op) = operator {
                warn!(?op, "unhandled operator");
            }

            Ok(Expr::UnOp(UnOp {
                kind: UnOpKind::Not,
                val: expr(value).map(Box::new)?,
            }))
        }
        Node::ArithmeticNode {
            left,
            right,
            operation,
        } => {
            let kind = match operation.as_str() {
                "add" => BinOpKind::Add,
                _ => bail!("Unknown arithmetic operation {operation}"),
            };
            Ok(Expr::BinOp(BinOp {
                kind,
                lhs: expr(left).map(Box::new)?,
                rhs: expr(right).map(Box::new)?,
            }))
        }
        Node::And { left, right } => Ok(Expr::BinOp(BinOp {
            kind: BinOpKind::And,
            lhs: expr(left).map(Box::new)?,
            rhs: expr(right).map(Box::new)?,
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
        Node::Dict { args } => Ok(Expr::Dict(Dict {
            args: args
                .as_argument()
                .ok_or_eyre("Expected argument node")?
                .kwargs
                .iter()
                .map(|pair| {
                    Ok((
                        pair.key
                            .as_string()
                            .ok_or_eyre("Expected string for dict key")?
                            .to_owned(),
                        expr(&pair.value)?,
                    ))
                })
                .collect::<eyre::Result<_>>()?,
            order: Vec::new(),
        })),
        _ => bail!("Unexpected expression node {node:?}"),
    }
}

fn method(source_object: &Node, name: &Node, method_args: &Node) -> eyre::Result<Method> {
    Ok(Method {
        obj: expr(source_object).map(Box::new)?,
        name: id(name)?,
        args: args(method_args)?,
    })
}

fn assign_stmt(name: &Node, value: &Node, is_plus: bool) -> eyre::Result<AssignStmt> {
    Ok(AssignStmt {
        name: id(name)?,
        value: expr(value)?,
        is_plus,
    })
}

fn id(node: &Node) -> eyre::Result<String> {
    Ok(node.as_id().ok_or_eyre("not an identifier")?.to_owned())
}

pub fn options(options: &[OptionNode]) -> ProjectOptions {
    options
        .iter()
        .map(|v| match v {
            OptionNode::Bool {
                name,
                value,
                description,
                deprecated,
            } => (
                name.clone(),
                ProjectOption {
                    description: description.clone(),
                    kind: ProjectOptionKind::Bool { value: *value },
                    deprecated: *deprecated,
                },
            ),
            OptionNode::Combo {
                name,
                value,
                choices,
                description,
                deprecated,
            } => (
                name.clone(),
                ProjectOption {
                    description: description.clone(),
                    kind: ProjectOptionKind::Combo {
                        choices: choices.clone(),
                        value: value.clone(),
                    },
                    deprecated: *deprecated,
                },
            ),
        })
        .collect()
}
