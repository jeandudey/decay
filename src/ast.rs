use std::collections::HashMap;

use eyre::{Ok, OptionExt, bail};

pub mod raw;

#[derive(Debug)]
pub struct CodeBlock {
    pub statements: Vec<Statement>,
}

#[derive(Debug)]
pub enum Statement {
    Function(Function),
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub args: Vec<Value>,
    pub kwargs: HashMap<String, Value>,
}

#[derive(Debug)]
pub enum Value {
    String(String),
    Array(Vec<Value>),
}

impl Value {
    fn from_raw(node: &raw::Node) -> eyre::Result<Self> {
        Ok(match node {
            raw::Node::String { value, .. } => Value::String(value.clone()),
            raw::Node::Array { args } => {
                let argument = args.as_argument().ok_or_eyre("expected argument node")?;
                Value::Array(
                    argument
                        .arguments
                        .iter()
                        .map(Value::from_raw)
                        .collect::<eyre::Result<Vec<_>>>()?,
                )
            }
            _ => bail!("Unexpected value node {node:?}"),
        })
    }

    pub fn as_string(&self) -> eyre::Result<&str> {
        match self {
            Self::String(v) => Ok(v),
            _ => bail!("expected string value"),
        }
    }
}

pub fn lower(ast: raw::Node) -> eyre::Result<CodeBlock> {
    match ast {
        raw::Node::CodeBlock { lines } => {
            let mut statements = Vec::new();
            for line in lines {
                match line {
                    raw::Node::Function { name, args } => {
                        let mut lowered_args = Vec::new();

                        let argument = args.as_argument().ok_or_eyre("expected argument node")?;
                        for arg in &argument.arguments {
                            lowered_args.push(Value::from_raw(arg)?);
                        }

                        let mut kwargs = HashMap::new();
                        for kwarg in &argument.kwargs {
                            let id = kwarg
                                .key
                                .as_id()
                                .ok_or_eyre("expected identifier for keyword argument")?
                                .to_owned();
                            kwargs.insert(id, Value::from_raw(&kwarg.value)?);
                        }
                        statements.push(Statement::Function(Function {
                            name: name
                                .as_id()
                                .ok_or_eyre("Function name is not an identifier")?
                                .to_owned(),
                            args: lowered_args,
                            kwargs,
                        }));
                    }
                    _ => (), /* eprintln!("unhandled line {line:?}") */
                }
            }
            Ok(CodeBlock { statements })
        }
        _ => todo!(),
    }
}

#[derive(Debug)]
pub struct MesonProject {
    pub name: String,
}

pub fn eval(ast: &CodeBlock) -> eyre::Result<MesonProject> {
    let mut meson_project = None;

    for statement in &ast.statements {
        match statement {
            Statement::Function(function) => match function.name.as_str() {
                "project" => {
                    let mut args = function.args.iter();
                    let name = args.next().ok_or_eyre("Missing project name")?;
                }
                "subdir" => {
                    let dir = function
                        .args
                        .get(0)
                        .ok_or_eyre("Expected directory name")?
                        .as_string()?;
                    println!("TODO GO INTO SUBDIR {dir:?}");
                }
                _ => todo!("{function:?}"),
            },
            _ => bail!("Unhandled statement {statement:?}"),
        }
    }

    meson_project.ok_or_eyre("Meson project not initialized")
}
