use std::collections::HashMap;

/// A presence condition: an index into an [`Arena`] of hash-consed boolean
/// nodes.
///
/// Conditions are built over *multi-valued* variables (a meson `combo` option,
/// the host system, ...) rather than plain booleans: the atom `Lit(v, i)` reads
/// "variable `v` took its `i`th choice". Keeping the choice structure instead of
/// one-hot booleans is what lets the backend lower a condition to nested
/// `select()`s whose keys are mutually exclusive by construction.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Pc(u32);

impl Pc {
    /// Also the [`Default`], so a freshly built structure starts out present in
    /// no configuration until something says otherwise.
    pub const FALSE: Self = Self(0);
    pub const TRUE: Self = Self(1);

    pub fn from_bool(value: bool) -> Self {
        if value { Self::TRUE } else { Self::FALSE }
    }

    pub fn is_false(&self) -> bool {
        *self == Self::FALSE
    }

    pub fn is_true(&self) -> bool {
        *self == Self::TRUE
    }

    pub fn is_const(&self) -> bool {
        self.is_true() || self.is_false()
    }

    pub fn index(&self) -> u32 {
        self.0
    }
}

/// A configuration variable.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VarId(u32);

impl VarId {
    pub fn index(&self) -> usize {
        self.0 as usize
    }

    /// Rebuild an id from a position in [`Arena::vars`], for callers that were
    /// handed the variable list rather than the arena.
    pub fn from_index(index: usize) -> Self {
        Self(index as u32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    False,
    True,
    Lit(VarId, u32),
    Not(Pc),
    And(Pc, Pc),
    Or(Pc, Pc),
}

/// What a configuration variable stands for.
///
/// The backend uses this to decide how a variable is materialized: a build
/// option becomes a user-facing constraint, a probe becomes an environment
/// capability flag, and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarKind {
    /// An option the project declares in its own option file.
    Option,
    /// One of meson's own build options, which every project has and none
    /// declares.
    BuiltinOption,
    /// A property of the machine being built for.
    Machine,
    /// The result of a toolchain/environment probe (`cc.has_header`, ...).
    Probe,
    /// Whether an external dependency is available.
    Dependency,
}

/// The declaration of a configuration variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    /// Unique key, e.g. `option:glx`.
    pub key: String,
    /// Human readable description, when the meson sources provided one.
    pub description: Option<String>,
    pub kind: VarKind,
    /// The values the variable may take. Always at least two.
    pub choices: Vec<String>,
    /// Index into `choices` of the value meson would pick by default.
    pub default: usize,
}

impl Var {
    pub fn choice_index(&self, name: &str) -> Option<u32> {
        self.choices.iter().position(|c| c == name).map(|i| i as u32)
    }
}

/// Hash-consed store of presence conditions.
#[derive(Debug)]
pub struct Arena {
    nodes: Vec<Node>,
    memo: HashMap<Node, Pc>,
    vars: Vec<Var>,
    by_key: HashMap<String, VarId>,
    restrict_memo: HashMap<(Pc, VarId, u32), Pc>,
    support_memo: HashMap<Pc, Vec<VarId>>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Arena {
    pub fn new() -> Self {
        let nodes = vec![Node::False, Node::True];

        debug_assert_eq!(nodes[Pc::FALSE.0 as usize], Node::False);
        debug_assert_eq!(nodes[Pc::TRUE.0 as usize], Node::True);

        let mut memo = HashMap::new();
        memo.insert(Node::False, Pc::FALSE);
        memo.insert(Node::True, Pc::TRUE);

        Self {
            nodes,
            memo,
            vars: Vec::new(),
            by_key: HashMap::new(),
            restrict_memo: HashMap::new(),
            support_memo: HashMap::new(),
        }
    }

    pub fn node(&self, pc: Pc) -> &Node {
        &self.nodes[pc.0 as usize]
    }

    pub fn vars(&self) -> &[Var] {
        &self.vars
    }

    pub fn var(&self, id: VarId) -> &Var {
        &self.vars[id.0 as usize]
    }

    pub fn var_id(&self, key: &str) -> Option<VarId> {
        self.by_key.get(key).copied()
    }

    /// Declare a variable, or return the existing one with the same key.
    ///
    /// Re-declaring with a different domain is a bug in the caller rather than
    /// something the sources can trigger, so the first declaration wins.
    pub fn declare(&mut self, var: Var) -> VarId {
        if let Some(id) = self.by_key.get(&var.key) {
            return *id;
        }
        debug_assert!(var.choices.len() >= 2, "`{}` has no real choice", var.key);
        let id = VarId(self.vars.len() as u32);
        self.by_key.insert(var.key.clone(), id);
        self.vars.push(var);
        id
    }

    pub fn lit(&mut self, var: VarId, choice: u32) -> Pc {
        debug_assert!((choice as usize) < self.vars[var.0 as usize].choices.len());
        self.intern(Node::Lit(var, choice))
    }

    pub fn not(&mut self, a: Pc) -> Pc {
        // Negation is kept as a node rather than pushed down to the leaves.
        // Rewriting it away would lose the syntactic link between a formula and
        // its negation, and that link is what makes the two sides of an `if`
        // cancel out without asking the solver.
        match self.nodes[a.0 as usize] {
            Node::True => Pc::FALSE,
            Node::False => Pc::TRUE,
            Node::Not(inner) => inner,
            _ => self.intern(Node::Not(a)),
        }
    }

    pub fn and(&mut self, a: Pc, b: Pc) -> Pc {
        if a.is_false() || b.is_false() {
            return Pc::FALSE;
        }
        if a.is_true() {
            return b;
        }
        if b.is_true() {
            return a;
        }
        if a == b {
            return a;
        }
        if self.is_negation(a, b) || self.conflicts(a, b) {
            return Pc::FALSE;
        }
        let (a, b) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        self.intern(Node::And(a, b))
    }

    pub fn or(&mut self, a: Pc, b: Pc) -> Pc {
        if a.is_true() || b.is_true() {
            return Pc::TRUE;
        }
        if a.is_false() {
            return b;
        }
        if b.is_false() {
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

    pub fn implies(&mut self, a: Pc, b: Pc) -> Pc {
        let na = self.not(a);
        self.or(na, b)
    }

    /// The value of `pc` once `var` is known to have taken `choice`.
    pub fn restrict(&mut self, pc: Pc, var: VarId, choice: u32) -> Pc {
        if pc.is_const() {
            return pc;
        }
        if let Some(&hit) = self.restrict_memo.get(&(pc, var, choice)) {
            return hit;
        }
        let out = match self.nodes[pc.0 as usize].clone() {
            Node::True | Node::False => pc,
            Node::Lit(v, c) if v == var => {
                if c == choice {
                    Pc::TRUE
                } else {
                    Pc::FALSE
                }
            }
            Node::Lit(..) => pc,
            Node::Not(inner) => {
                let inner = self.restrict(inner, var, choice);
                self.not(inner)
            }
            Node::And(x, y) => {
                let x = self.restrict(x, var, choice);
                let y = self.restrict(y, var, choice);
                self.and(x, y)
            }
            Node::Or(x, y) => {
                let x = self.restrict(x, var, choice);
                let y = self.restrict(y, var, choice);
                self.or(x, y)
            }
        };
        self.restrict_memo.insert((pc, var, choice), out);
        out
    }

    /// The variables `pc` actually mentions, in first-mention order.
    pub fn support(&mut self, pc: Pc) -> Vec<VarId> {
        if let Some(hit) = self.support_memo.get(&pc) {
            return hit.clone();
        }
        let out = match self.nodes[pc.0 as usize].clone() {
            Node::True | Node::False => Vec::new(),
            Node::Lit(v, _) => vec![v],
            Node::Not(inner) => self.support(inner),
            Node::And(x, y) | Node::Or(x, y) => {
                let mut out = self.support(x);
                for v in self.support(y) {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
                out
            }
        };
        self.support_memo.insert(pc, out.clone());
        out
    }

    /// Cheap syntactic check for `a` being the negation of `b`.
    fn is_negation(&self, a: Pc, b: Pc) -> bool {
        matches!(self.nodes[a.0 as usize], Node::Not(x) if x == b)
            || matches!(self.nodes[b.0 as usize], Node::Not(x) if x == a)
    }

    /// Two literals over the same variable but different choices can never hold
    /// together. Catching this here keeps the common `x == 'a' and x == 'b'`
    /// dead branch out of the solver.
    fn conflicts(&self, a: Pc, b: Pc) -> bool {
        matches!(
            (&self.nodes[a.0 as usize], &self.nodes[b.0 as usize]),
            (Node::Lit(v, i), Node::Lit(w, j)) if v == w && i != j
        )
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
