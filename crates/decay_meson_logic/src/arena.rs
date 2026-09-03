use std::collections::HashMap;

/// A presence condition.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Pc(u32);

impl Pc {
    pub const FALSE: Self = Self(0);
    pub const TRUE: Self = Self(1);

    pub fn is_false(&self) -> bool {
        *self == Self::FALSE
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct VarId(u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    True,
    False,
    Var(VarId),
    Not(Pc),
    And(Pc, Pc),
    Or(Pc, Pc),
}

#[derive(Debug)]
pub struct Arena {
    nodes: Vec<Node>,
    memo: HashMap<Node, Pc>,
    names: Vec<String>,
    by_name: HashMap<String, VarId>,
}

impl Arena {
    pub fn new() -> Self {
        let mut nodes = Vec::new();
        nodes.push(Node::False);
        nodes.push(Node::True);

        debug_assert_eq!(nodes[Pc::FALSE.0 as usize], Node::False);
        debug_assert_eq!(nodes[Pc::TRUE.0 as usize], Node::True);

        let mut memo = HashMap::new();
        memo.insert(Node::False, Pc::FALSE);
        memo.insert(Node::True, Pc::TRUE);

        Self {
            nodes,
            memo,
            names: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn and(&mut self, a: Pc, b: Pc) -> Pc {
        if a == Pc::FALSE || b == Pc::FALSE {
            return Pc::FALSE;
        };
        if a == Pc::TRUE {
            return b;
        }
        if b == Pc::TRUE {
            return a;
        }
        if a == b {
            return a;
        }
        let (a, b) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        self.intern(Node::And(a, b))
    }

    pub fn or(&mut self, a: Pc, b: Pc) -> Pc {
        if a == Pc::TRUE || b == Pc::TRUE {
            return Pc::TRUE;
        }
        if a == Pc::FALSE {
            return b;
        }
        if b == Pc::FALSE {
            return a;
        }
        if a == b {
            return a;
        }
        if self.is_negation(a, b) {
            return Pc::TRUE;
        }
        let (a, b) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        self.intern(Node::Or(a, b))
    }

    fn is_negation(&self, a: Pc, b: Pc) -> bool {
        matches!(self.nodes[a.0 as usize], Node::Not(x) if x == b)
            || matches!(self.nodes[b.0 as usize], Node::Not(x) if x == a)
    }

    fn intern(&mut self, node: Node) -> Pc {
        if let Some(&pc) = self.memo.get(&node) {
            return pc;
        }
        let id = Pc(self.nodes.len() as u32);
        self.nodes.push(node.clone());
        self.memo.insert(node, id);
        id
    }
}
