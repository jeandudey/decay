use {
    crate::{
        arena::VarId,
        solver::Solver, //
    },
    z3::{
        SatResult,
        ast::{
            Bool,
            Int, //
        },
    },
};

/// Encodes each multi-valued variable as an integer constant confined to
/// `0..n`, so `Lit(v, i)` is simply `v == i` and the mutual exclusion between
/// choices comes for free.
#[derive(Debug)]
pub struct Z3Solver {
    solver: z3::Solver,
    consts: Vec<Option<Int>>,
}

impl Default for Z3Solver {
    fn default() -> Self {
        Self::new()
    }
}

impl Z3Solver {
    pub fn new() -> Self {
        Self {
            solver: z3::Solver::new(),
            consts: Vec::new(),
        }
    }
}

impl Solver for Z3Solver {
    type Term = Bool;

    fn declare(&mut self, var: VarId, n_choices: u32) {
        if self.consts.len() <= var.index() {
            self.consts.resize(var.index() + 1, None);
        }
        if self.consts[var.index()].is_some() {
            return;
        }
        let c = Int::new_const(format!("v{}", var.index()));
        self.solver.assert(c.ge(Int::from_i64(0)));
        self.solver.assert(c.lt(Int::from_i64(i64::from(n_choices))));
        self.consts[var.index()] = Some(c);
    }

    fn lit(&mut self, var: VarId, choice: u32) -> Self::Term {
        let c = self.consts[var.index()]
            .as_ref()
            .expect("variable used before it was declared");
        c.eq(Int::from_i64(i64::from(choice)))
    }

    fn top(&mut self) -> Self::Term {
        Bool::from_bool(true)
    }

    fn bottom(&mut self) -> Self::Term {
        Bool::from_bool(false)
    }

    fn not(&mut self, t: &Self::Term) -> Self::Term {
        !t
    }

    fn and(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term {
        Bool::and(&[a.clone(), b.clone()])
    }

    fn or(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term {
        Bool::or(&[a.clone(), b.clone()])
    }

    fn assume(&mut self, t: &Self::Term) {
        self.solver.assert(t);
    }

    fn is_sat(&mut self, t: &Self::Term) -> bool {
        matches!(
            self.solver.check_assumptions(&[t.clone()]),
            SatResult::Sat //
        )
    }
}
