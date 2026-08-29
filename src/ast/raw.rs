use {
    eyre::{
        Context,
        OptionExt, //
    },
    pyo3::{
        ffi::c_str,
        prelude::*, //
    },
    serde::Deserialize,
    std::{
        ffi::CStr,
        path::Path, //
    }, //
};

static PARSE_PY: &CStr = c_str!(include_str!("parse.py"));
static PARSE_OPTIONS_PY: &CStr = c_str!(include_str!("parse_options.py"));

pub fn parse(path: impl AsRef<Path>) -> eyre::Result<Node> {
    let json = Python::attach(|py| {
        let module = PyModule::from_code(py, PARSE_PY, c"parse.py", c"parse")
            .wrap_err("Failed to load parse.py module")?;
        let json = module
            .getattr("parse")?
            .call1((path
                .as_ref()
                .to_str()
                .ok_or_eyre("Failed to convert path into a string")?,))?
            .extract::<String>()?;
        Ok::<_, eyre::Report>(json)
    })?;

    serde_json::from_str(&json).wrap_err("Failed to parse JSON AST")
}

pub fn parse_options(path: impl AsRef<Path>) -> eyre::Result<Vec<OptionNode>> {
    let json = Python::attach(|py| {
        let module =
            PyModule::from_code(py, PARSE_OPTIONS_PY, c"parse_options.py", c"parse_options")
                .inspect_err(|err| err.print(py))
                .wrap_err("Failed to load parse_options.py module")?;
        let json = module
            .getattr("parse")?
            .call1((path
                .as_ref()
                .to_str()
                .ok_or_eyre("Failed to convert path into a string")?,))?
            .extract::<String>()?;
        Ok::<_, eyre::Report>(json)
    })?;

    serde_json::from_str(&json).wrap_err("Failed to parse JSON AST")
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum Node {
    #[serde(rename = "ArgumentNode")]
    Argument(Argument),
    #[serde(rename = "ArrayNode")]
    Array { args: Box<Node> },
    #[serde(rename = "AssignmentNode")]
    Assignment {
        operator: Box<Option<Node>>,
        value: Box<Node>,
        var_name: Box<Node>,
    },
    #[serde(rename = "BooleanNode")]
    Boolean { value: bool },
    #[serde(rename = "CodeBlockNode")]
    CodeBlock {
        #[serde(default)]
        lines: Vec<Node>,
    },
    #[serde(rename = "ComparisonNode")]
    Comparison {
        ctype: String,
        left: Box<Node>,
        operator: Option<Box<Node>>,
        right: Box<Node>,
    },
    #[serde(rename = "ElseNode")]
    Else { block: Box<Node> },
    #[serde(rename = "EmptyNode")]
    Empty,
    #[serde(rename = "FunctionNode")]
    Function {
        #[serde(rename = "func_name")]
        name: Box<Node>,
        args: Box<Node>,
    },
    #[serde(rename = "IdNode")]
    Id { value: String },
    #[serde(rename = "IfClauseNode")]
    IfClause {
        elseblock: Box<Node>,
        #[serde(default)]
        ifs: Vec<Node>,
    },
    #[serde(rename = "IfNode")]
    If {
        block: Box<Node>,
        condition: Box<Node>,
    },
    #[serde(rename = "IndexNode")]
    Index {
        index: Box<Node>,
        iobject: Box<Node>,
    },
    #[serde(rename = "MethodNode")]
    Method {
        args: Box<Node>,
        name: Box<Node>,
        source_object: Box<Node>,
    },
    #[serde(rename = "NotNode")]
    Not {
        operator: Option<Box<Node>>,
        value: Box<Node>,
    },
    #[serde(rename = "NumberNode")]
    Number { raw_value: String, value: i64 },
    #[serde(rename = "OrNode")]
    Or {
        left: Box<Node>,
        operator: Option<Box<Node>>,
        right: Box<Node>,
    },
    #[serde(rename = "PlusAssignmentNode")]
    PlusAssignment {
        operator: Option<Box<Node>>,
        value: Box<Node>,
        var_name: Box<Node>,
    },
    #[serde(rename = "StringNode")]
    String {
        is_fstring: bool,
        raw_value: String,
        value: String,
    },
    #[serde(rename = "SymbolNode")]
    Symbol { value: String },
    #[serde(rename = "WhitespaceNode")]
    Whitespace {
        block_indent: bool,
        is_continuation: bool,
        value: String,
    },
}

impl Node {
    pub fn as_id(&self) -> Option<&str> {
        match self {
            Node::Id { value } => Some(value),
            _ => None,
        }
    }

    pub fn as_argument(&self) -> Option<&Argument> {
        match self {
            Node::Argument(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_code_block(&self) -> Option<&[Node]> {
        match self {
            Node::CodeBlock { lines } => Some(lines),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Pair {
    pub key: Node,
    pub value: Node,
}

#[derive(Debug, Deserialize)]
pub struct Argument {
    #[serde(default)]
    pub arguments: Vec<Node>,
    #[serde(default)]
    pub kwargs: Vec<Pair>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum OptionNode {
    #[serde(rename = "UserBooleanOption")]
    Bool {
        name: String,
        value: bool,
        #[serde(default)]
        description: Option<String>,
        deprecated: bool,
    },
    #[serde(rename = "UserComboOption")]
    Combo {
        name: String,
        value: String,
        choices: Vec<String>,
        description: Option<String>,
        deprecated: bool,
    },
}
