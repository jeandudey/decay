use {
    crate::{
        arena::{
            Arena,
            Node,
            Pc,
            Var,
            VarId, //
        },
        solver::Solver,
        stats,
    },
    std::{
        collections::HashMap,
        time::Instant, //
    },
};

/// The presence-condition algebra used by the executor: a hash-consed [`Arena`]
/// for cheap structural work, backed by a [`Solver`] for the questions that
/// need real reasoning (is this path reachable at all?).
#[derive(Debug)]
pub struct Logic<S: Solver> {
    solver: S,
    arena: Arena,
    terms: HashMap<Pc, S::Term>,
    /// Satisfiability memo keyed by `Pc` identity.
    sat: HashMap<Pc, bool>,
    /// Satisfiability memo keyed by literal set, for the common case where the
    /// condition is a plain conjunction. Structurally distinct `Pc`s that mean
    /// the same conjunction share an entry, so a guard checked from many
    /// statements costs one solver call.
    conj_sat: HashMap<Box<[(VarId, u32, bool)]>, bool>,
}

impl<S: Solver> Logic<S> {
    pub fn new(solver: S) -> Self {
        Self {
            solver,
            arena: Arena::new(),
            terms: HashMap::new(),
            sat: HashMap::new(),
            conj_sat: HashMap::new(),
        }
    }

    pub fn arena(&self) -> &Arena {
        &self.arena
    }

    pub fn arena_mut(&mut self) -> &mut Arena {
        &mut self.arena
    }

    pub fn into_arena(self) -> Arena {
        self.arena
    }

    pub fn vars(&self) -> &[Var] {
        self.arena.vars()
    }

    pub fn var(&self, id: VarId) -> &Var {
        self.arena.var(id)
    }

    pub fn declare(&mut self, var: Var) -> VarId {
        let n = var.choices.len() as u32;
        let id = self.arena.declare(var);
        self.solver.declare(id, n);
        id
    }

    pub fn lit(&mut self, var: VarId, choice: u32) -> Pc {
        self.arena.lit(var, choice)
    }

    /// The condition for `var` holding one of `choices`.
    pub fn any_of(&mut self, var: VarId, choices: impl IntoIterator<Item = u32>) -> Pc {
        let mut out = Pc::FALSE;
        for c in choices {
            let l = self.arena.lit(var, c);
            out = self.arena.or(out, l);
        }
        out
    }

    pub fn and(&mut self, a: Pc, b: Pc) -> Pc {
        self.arena.and(a, b)
    }

    pub fn or(&mut self, a: Pc, b: Pc) -> Pc {
        self.arena.or(a, b)
    }

    pub fn not(&mut self, a: Pc) -> Pc {
        self.arena.not(a)
    }

    pub fn implies(&mut self, a: Pc, b: Pc) -> Pc {
        self.arena.implies(a, b)
    }

    pub fn restrict(&mut self, pc: Pc, var: VarId, choice: u32) -> Pc {
        self.arena.restrict(pc, var, choice)
    }

    pub fn support(&mut self, pc: Pc) -> Vec<VarId> {
        self.arena.support(pc)
    }

    /// Rule out every configuration in which `pc` fails to hold.
    ///
    /// `error()` under a guard is the motivating case: meson refusing to
    /// configure means those option combinations simply do not exist.
    pub fn assume(&mut self, pc: Pc) {
        stats::bump(&stats::ASSUME_CALLS);
        let t = self.term_timed(pc);
        self.solver.assume(&t);
        // Adding a constraint can only shrink the space: an unsatisfiable
        // condition stays unsatisfiable, so keep the `false` entries and drop
        // only the ones that might now be ruled out.
        let before = self.sat.len() + self.conj_sat.len();
        self.sat.retain(|_, &mut sat| !sat);
        self.conj_sat.retain(|_, &mut sat| !sat);
        stats::add(
            &stats::SAT_MEMO_DROPPED,
            (before - self.sat.len() - self.conj_sat.len()) as u64,
        );
    }

    pub fn is_sat(&mut self, pc: Pc) -> bool {
        stats::bump(&stats::IS_SAT_CALLS);
        if pc.is_false() {
            stats::bump(&stats::IS_SAT_CONST);
            return false;
        }
        if pc.is_true() {
            stats::bump(&stats::IS_SAT_CONST);
            return true;
        }
        if let Some(&hit) = self.sat.get(&pc) {
            stats::bump(&stats::IS_SAT_HIT);
            return hit;
        }

        // Most `is_sat` calls are on a plain conjunction of literals. A
        // conjunction that names one variable at two values is unsatisfiable
        // outright; otherwise its answer is shared by every `Pc` with the same
        // literal set, whichever way it was built.
        let conj = self.arena.conj_lits(pc);
        if let Some(lits) = &conj {
            if self.arena.conj_is_unsat(lits) {
                stats::bump(&stats::IS_SAT_HIT);
                self.sat.insert(pc, false);
                return false;
            }
            if let Some(&hit) = self.conj_sat.get(lits) {
                stats::bump(&stats::IS_SAT_HIT);
                self.sat.insert(pc, hit);
                return hit;
            }
        }

        stats::bump(&stats::IS_SAT_MISS);
        let t = self.term_timed(pc);
        let r = self.solver.is_sat(&t);
        self.sat.insert(pc, r);
        if let Some(lits) = conj {
            self.conj_sat.insert(lits, r);
        }
        stats::maybe_report();
        r
    }

    /// Whether `a` holds in every configuration in which `b` does.
    pub fn entails(&mut self, b: Pc, a: Pc) -> bool {
        let na = self.not(a);
        let counter = self.and(b, na);
        !self.is_sat(counter)
    }

    pub fn equivalent(&mut self, a: Pc, b: Pc) -> bool {
        a == b || (self.entails(a, b) && self.entails(b, a))
    }

    /// [`Self::term`] with the top-level call counted and timed (recursion
    /// included). Only the entry points that precede a solver call use this.
    fn term_timed(&mut self, pc: Pc) -> S::Term {
        stats::bump(&stats::TERM_TOP_CALLS);
        if self.terms.contains_key(&pc) {
            stats::bump(&stats::TERM_TOP_HIT);
        }
        let start = Instant::now();
        let t = self.term(pc);
        stats::add(&stats::TERM_NANOS, start.elapsed().as_nanos() as u64);
        t
    }

    fn term(&mut self, pc: Pc) -> S::Term {
        if let Some(t) = self.terms.get(&pc) {
            return t.clone();
        }
        let t = match self.arena.node(pc).clone() {
            Node::True => self.solver.top(),
            Node::False => self.solver.bottom(),
            Node::Lit(v, c) => self.solver.lit(v, c),
            Node::Not(inner) => {
                let inner = self.term(inner);
                self.solver.not(&inner)
            }
            Node::And(x, y) => {
                let (x, y) = (self.term(x), self.term(y));
                self.solver.and(&x, &y)
            }
            Node::Or(x, y) => {
                let (x, y) = (self.term(x), self.term(y));
                self.solver.or(&x, &y)
            }
        };
        self.terms.insert(pc, t.clone());
        t
    }
}
