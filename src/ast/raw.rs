use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Pair {
    pub key: Node,
    pub value: Node,
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
        operator: Box<Option<Node>>,
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
        operator: Box<Option<Node>>,
        value: Box<Node>,
    },
    #[serde(rename = "NumberNode")]
    Number { raw_value: String, value: i64 },
    #[serde(rename = "OrNode")]
    Or {
        left: Box<Node>,
        operator: Box<Option<Node>>,
        right: Box<Node>,
    },
    #[serde(rename = "PlusAssignmentNode")]
    PlusAssignment {
        operator: Box<Option<Node>>,
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
}

#[derive(Debug, Deserialize)]
pub struct Argument {
    #[serde(default)]
    pub arguments: Vec<Node>,
    #[serde(default)]
    pub kwargs: Vec<Pair>,
}
