use {
    crate::{
        arena::{
            Arena,
            Node,
            Pc,
            Var,
            VarId, //
        },
        logic::Logic,
        solver::Solver,
    },
    std::collections::HashMap,
};

/// A presence condition lifted out of one [`Arena`] so another can read it
/// back: every variable it mentions travels with it as a full declaration,
/// and each literal names its choice by value rather than by index.
///
/// A `Pc` means nothing outside the arena it was built in, and each imported
/// project evaluates against its own. This is how one project's condition
/// (the configurations a sibling's `pkg.generate()` actually runs under)
/// reaches another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    vars: Vec<Var>,
    /// Topologically ordered; the last node is the root.
    nodes: Vec<FNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FNode {
    True,
    False,
    /// Index into `vars`, index into that variable's `choices`.
    Lit(usize, usize),
    Not(usize),
    And(usize, usize),
    Or(usize, usize),
}

impl Formula {
    pub const TRUE: Formula = Formula {
        vars: Vec::new(),
        nodes: Vec::new(),
    };

    pub fn is_true(&self) -> bool {
        matches!(self.nodes.last(), None | Some(FNode::True))
    }

    /// Every variable the formula mentions.
    pub fn vars(&self) -> &[Var] {
        &self.vars
    }
}

impl Arena {
    /// Lift `pc` out of this arena, keeping only the variables `keep`
    /// accepts. Every other variable is quantified out existentially: the
    /// result holds wherever *some* choice of the dropped variables makes
    /// `pc` hold. Returns the formula and the keys of what was dropped.
    pub fn export(&mut self, pc: Pc, keep: impl Fn(&Var) -> bool) -> (Formula, Vec<String>) {
        let mut pc = pc;
        let mut dropped = Vec::new();
        for var in self.support(pc) {
            if keep(self.var(var)) {
                continue;
            }
            dropped.push(self.var(var).key.clone());
            let mut any = Pc::FALSE;
            for choice in 0..self.var(var).choices.len() as u32 {
                let r = self.restrict(pc, var, choice);
                any = self.or(any, r);
            }
            pc = any;
        }

        let mut out = Formula {
            vars: Vec::new(),
            nodes: Vec::new(),
        };
        let mut var_idx: HashMap<VarId, usize> = HashMap::new();
        let mut memo: HashMap<Pc, usize> = HashMap::new();
        self.export_node(pc, &mut out, &mut var_idx, &mut memo);
        (out, dropped)
    }

    fn export_node(
        &self,
        pc: Pc,
        out: &mut Formula,
        var_idx: &mut HashMap<VarId, usize>,
        memo: &mut HashMap<Pc, usize>,
    ) -> usize {
        if let Some(&i) = memo.get(&pc) {
            return i;
        }
        let node = match *self.node(pc) {
            Node::True => FNode::True,
            Node::False => FNode::False,
            Node::Lit(var, choice) => {
                let v = *var_idx.entry(var).or_insert_with(|| {
                    out.vars.push(self.var(var).clone());
                    out.vars.len() - 1
                });
                FNode::Lit(v, choice as usize)
            }
            Node::Not(a) => FNode::Not(self.export_node(a, out, var_idx, memo)),
            Node::And(a, b) => {
                let a = self.export_node(a, out, var_idx, memo);
                FNode::And(a, self.export_node(b, out, var_idx, memo))
            }
            Node::Or(a, b) => {
                let a = self.export_node(a, out, var_idx, memo);
                FNode::Or(a, self.export_node(b, out, var_idx, memo))
            }
        };
        out.nodes.push(node);
        memo.insert(pc, out.nodes.len() - 1);
        out.nodes.len() - 1
    }
}

impl<S: Solver> Logic<S> {
    /// Read a [`Formula`] back into this arena. A variable already declared
    /// under the same key is the same variable; its choices are matched by
    /// value. Fails, naming the variable, when a choice the formula uses is
    /// not one this arena's declaration has.
    pub fn import(&mut self, formula: &Formula) -> Result<Pc, String> {
        let mut ids = Vec::with_capacity(formula.vars.len());
        for var in &formula.vars {
            ids.push(self.declare(var.clone()));
        }
        let mut pcs: Vec<Pc> = Vec::with_capacity(formula.nodes.len());
        for node in &formula.nodes {
            let pc = match *node {
                FNode::True => Pc::TRUE,
                FNode::False => Pc::FALSE,
                FNode::Lit(v, choice) => {
                    let name = &formula.vars[v].choices[choice];
                    let here = self.var(ids[v]).choice_index(name).ok_or_else(|| {
                        format!("`{}` has no choice `{name}` here", formula.vars[v].key)
                    })?;
                    self.lit(ids[v], here)
                }
                FNode::Not(a) => self.not(pcs[a]),
                FNode::And(a, b) => self.and(pcs[a], pcs[b]),
                FNode::Or(a, b) => self.or(pcs[a], pcs[b]),
            };
            pcs.push(pc);
        }
        Ok(pcs.last().copied().unwrap_or(Pc::TRUE))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            VarKind,
            Z3Solver, //
        },
    };

    fn var(key: &str, kind: VarKind, choices: &[&str]) -> Var {
        Var {
            key: key.to_owned(),
            description: None,
            kind,
            choices: choices.iter().map(|c| c.to_string()).collect(),
            default: 0,
        }
    }

    #[test]
    fn round_trips_by_choice_name_and_drops_what_it_must() {
        let mut a = Logic::new(Z3Solver::new());
        let sys = a.declare(var(
            "machine:host:system",
            VarKind::Machine,
            &["linux", "windows", "darwin"],
        ));
        let opt = a.declare(var("option:extra", VarKind::Option, &["true", "false"]));
        let (linux, darwin) = (a.lit(sys, 0), a.lit(sys, 2));
        let (on, off) = (a.lit(opt, 0), a.lit(opt, 1));
        let x = a.and(linux, on);
        let y = a.and(darwin, off);
        let pc = a.or(x, y);

        let (formula, dropped) = a.arena_mut().export(pc, |v| v.kind != VarKind::Option);
        assert_eq!(dropped, ["option:extra"]);
        assert_eq!(formula.vars().len(), 1);

        // Same key, choices in another order: matched by name, not index.
        let mut b = Logic::new(Z3Solver::new());
        let sys_b = b.declare(var(
            "machine:host:system",
            VarKind::Machine,
            &["darwin", "linux", "windows"],
        ));
        let got = b.import(&formula).unwrap();
        for (choice, expect) in [(0, true), (1, true), (2, false)] {
            let lit = b.lit(sys_b, choice);
            let both = b.and(got, lit);
            assert_eq!(b.is_sat(both), expect, "choice {choice}");
        }
    }

    #[test]
    fn a_choice_missing_here_is_an_error() {
        let mut a = Logic::new(Z3Solver::new());
        let sys = a.declare(var(
            "machine:host:system",
            VarKind::Machine,
            &["linux", "haiku"],
        ));
        let haiku = a.lit(sys, 1);
        let (formula, _) = a.arena_mut().export(haiku, |_| true);

        let mut b = Logic::new(Z3Solver::new());
        b.declare(var(
            "machine:host:system",
            VarKind::Machine,
            &["linux", "windows"],
        ));
        assert!(b.import(&formula).unwrap_err().contains("haiku"));
    }
}
