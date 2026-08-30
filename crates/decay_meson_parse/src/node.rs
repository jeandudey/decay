use eyre::bail;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum Node {
    #[serde(rename = "ArgumentNode")]
    Argument(Argument),
    #[serde(rename = "ArrayNode")]
    Array { args: Box<Node> },
    #[serde(rename = "AssignmentNode")]
    Assignment {
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
    Not { value: Box<Node> },
    #[serde(rename = "NumberNode")]
    Number { value: i64 },
    #[serde(rename = "OrNode")]
    Or { left: Box<Node>, right: Box<Node> },
    #[serde(rename = "PlusAssignmentNode")]
    PlusAssignment {
        value: Box<Node>,
        var_name: Box<Node>,
    },
    #[serde(rename = "StringNode")]
    String { is_fstring: bool, value: String },
    #[serde(rename = "ForeachClauseNode")]
    ForeachClause {
        varnames: Vec<Node>,
        items: Box<Node>,
        #[serde(rename = "block")]
        body: Box<Node>,
    },
    #[serde(rename = "ArithmeticNode")]
    ArithmeticNode {
        left: Box<Node>,
        right: Box<Node>,
        operation: String,
    },
    #[serde(rename = "AndNode")]
    And { left: Box<Node>, right: Box<Node> },
    #[serde(rename = "TernaryNode")]
    Ternary {
        condition: Box<Node>,
        trueblock: Box<Node>,
        falseblock: Box<Node>,
    },
    #[serde(rename = "DictNode")]
    Dict { args: Box<Node> },
}

impl Node {
    pub(crate) fn expect_id(&self) -> eyre::Result<String> {
        match self {
            Node::Id { value } => Ok(value.clone()),
            _ => bail!("Expected an Id node"),
        }
    }

    pub(crate) fn expect_string(&self) -> eyre::Result<String> {
        match self {
            Node::String { value, .. } => Ok(value.clone()),
            _ => bail!("Expected a String node"),
        }
    }

    pub(crate) fn expect_argument(&self) -> eyre::Result<&Argument> {
        match self {
            Node::Argument(v) => Ok(v),
            _ => bail!("Expected an Argument node"),
        }
    }

    pub(crate) fn as_code_block(&self) -> Option<&[Node]> {
        match self {
            Node::CodeBlock { lines } => Some(lines),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Pair {
    pub key: Node,
    pub value: Node,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Argument {
    #[serde(default)]
    pub arguments: Vec<Node>,
    #[serde(default)]
    pub kwargs: Vec<Pair>,
}
