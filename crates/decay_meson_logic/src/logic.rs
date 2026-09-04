use {
    crate::{
        arena::{
            Arena,
            Pc,
            Var,
            VarId, //
        },
        stats,
    },
    std::{
        collections::HashMap,
        time::Instant, //
    },
};

/// The presence-condition algebra used by the executor: a hash-consed [`Arena`]
/// of reduced ordered BDDs. Structural work and the "is this path reachable at
/// all?" question are both answered by the diagrams directly — there is no
/// separate solver.
#[derive(Debug, Default)]
pub struct Logic {
    arena: Arena,
    /// Memoised [`Self::is_sat`] answers. Cleared of its *satisfiable* entries
    /// whenever [`Self::assume`] narrows the space — an unsatisfiable condition
    /// stays unsatisfiable when a constraint is added, so those entries are
    /// kept.
    sat: HashMap<Pc, bool>,
}

impl Logic {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn vars(&self) -> &[Var] {
        self.arena.vars()
    }

    pub fn var(&self, id: VarId) -> &Var {
        self.arena.var(id)
    }

    pub fn var_id(&self, key: &str) -> Option<VarId> {
        self.arena.var_id(key)
    }

    pub fn declare(&mut self, var: Var) -> VarId {
        self.arena.declare(var)
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

    /// The BDD node count of `pc`.
    pub fn size(&self, pc: Pc) -> usize {
        self.arena.size(pc)
    }

    /// Rule out every configuration in which `pc` fails to hold.
    ///
    /// `error()` under a guard is the motivating case: meson refusing to
    /// configure means those option combinations simply do not exist.
    pub fn assume(&mut self, pc: Pc) {
        stats::bump(&stats::ASSUME_CALLS);
        self.arena.assume(pc);
        let before = self.sat.len();
        self.sat.retain(|_, &mut sat| !sat);
        stats::add(&stats::SAT_MEMO_DROPPED, (before - self.sat.len()) as u64);
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
        stats::bump(&stats::IS_SAT_MISS);
        stats::bump(&stats::CHECK_CALLS);
        let start = Instant::now();
        let r = self.arena.is_sat(pc);
        stats::add(&stats::CHECK_NANOS, start.elapsed().as_nanos() as u64);
        self.sat.insert(pc, r);
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
}
