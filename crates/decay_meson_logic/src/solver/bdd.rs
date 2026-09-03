use {
    crate::{
        arena::VarId,
        solver::Solver,
        stats, //
    },
    biodivine_lib_bdd::{
        Bdd,
        BddVariable,
        BddVariableSet, //
    },
    std::time::Instant,
};

/// How many BDD variables to reserve. Each configuration variable claims
/// `ceil(log2(choices))` of them, so this is the ceiling on
/// `sum over vars of ceil(log2(choices))` for one project. Generous for the
/// biggest meson trees; a project that needs more trips the assert in
/// [`BddSolver::declare`], which is the signal to raise it (max is `u16::MAX`).
const BDD_VARS: u16 = 8192;

/// A [`Solver`] backed by reduced ordered binary decision diagrams.
///
/// The configuration space here is tiny and finite — tens to low thousands of
/// small-domain variables — so satisfiability is structural: `is_sat(t)` is
/// `ctx ∧ t ≠ ⊥`, with no search. Each multi-valued variable is binary-encoded
/// into `ceil(log2(n))` BDD variables; `Lit(v, i)` is the conjunction of those
/// bits spelling `i`, and the "index < n" domain constraint for a non-power-of-
/// two `n` is folded into `ctx` once at declare time, which is also where
/// [`assume`](Solver::assume) accumulates.
#[derive(Debug)]
pub struct BddSolver {
    vars: BddVariableSet,
    /// `bits[v]` = (first BDD variable, count) encoding configuration variable
    /// `v`. Dense and in `VarId` order.
    bits: Vec<(u16, u8)>,
    /// Next unclaimed BDD variable.
    next: u16,
    /// Everything ruled out so far: the domain constraints plus every
    /// `assume`d term.
    ctx: Bdd,
}

impl Default for BddSolver {
    fn default() -> Self {
        Self::new()
    }
}

impl BddSolver {
    pub fn new() -> Self {
        let vars = BddVariableSet::new_anonymous(BDD_VARS);
        let ctx = vars.mk_true();
        Self {
            vars,
            bits: Vec::new(),
            next: 0,
            ctx,
        }
    }

    /// The BDD for "the `count` bits from `start` spell `value`", LSB first.
    fn encode(&self, start: u16, count: u8, value: u32) -> Bdd {
        let mut b = self.vars.mk_true();
        for i in 0..count {
            let bit = (value >> i) & 1 == 1;
            let v = BddVariable::from_index((start + u16::from(i)) as usize);
            b = b.and(&self.vars.mk_literal(v, bit));
        }
        b
    }
}

/// Bits needed to number `n` choices: `ceil(log2(n))`, at least 1.
fn width(n: u32) -> u8 {
    debug_assert!(n >= 2);
    (32 - (n - 1).leading_zeros()).max(1) as u8
}

impl Solver for BddSolver {
    type Term = Bdd;

    fn declare(&mut self, var: VarId, n_choices: u32) {
        let idx = var.index();
        if idx < self.bits.len() {
            return;
        }
        assert_eq!(idx, self.bits.len(), "configuration variables declared out of order");

        let count = width(n_choices);
        let start = self.next;
        self.next = self
            .next
            .checked_add(u16::from(count))
            .filter(|&n| n <= BDD_VARS)
            .unwrap_or_else(|| {
                panic!(
                    "BDD variable pool exhausted at {BDD_VARS}; raise `BDD_VARS` in \
                     decay_meson_logic::solver::bdd"
                )
            });
        self.bits.push((start, count));
        stats::bump(&stats::VAR_DECLARE);

        // Rule out the bit patterns past `n_choices` when it is not a power of
        // two, so a variable can only ever hold a value it actually has.
        if !n_choices.is_power_of_two() {
            let mut legal = self.vars.mk_false();
            for value in 0..n_choices {
                legal = legal.or(&self.encode(start, count, value));
            }
            self.ctx = self.ctx.and(&legal);
        }
    }

    fn lit(&mut self, var: VarId, choice: u32) -> Self::Term {
        stats::bump(&stats::TERM_LIT);
        let (start, count) = self.bits[var.index()];
        self.encode(start, count, choice)
    }

    fn top(&mut self) -> Self::Term {
        self.vars.mk_true()
    }

    fn bottom(&mut self) -> Self::Term {
        self.vars.mk_false()
    }

    fn not(&mut self, t: &Self::Term) -> Self::Term {
        stats::bump(&stats::TERM_NOT);
        t.not()
    }

    fn and(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term {
        stats::bump(&stats::TERM_AND);
        a.and(b)
    }

    fn or(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term {
        stats::bump(&stats::TERM_OR);
        a.or(b)
    }

    fn assume(&mut self, t: &Self::Term) {
        self.ctx = self.ctx.and(t);
    }

    fn is_sat(&mut self, t: &Self::Term) -> bool {
        stats::bump(&stats::CHECK_CALLS);
        let start = Instant::now();
        let sat = !self.ctx.and(t).is_false();
        stats::add(&stats::CHECK_NANOS, start.elapsed().as_nanos() as u64);
        sat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Declare `n` variables with the given choice counts and return the solver.
    fn solver(choices: &[u32]) -> BddSolver {
        let mut s = BddSolver::new();
        for (i, &n) in choices.iter().enumerate() {
            s.declare(VarId::from_index(i), n);
        }
        s
    }

    #[test]
    fn width_is_ceil_log2() {
        assert_eq!(width(2), 1);
        assert_eq!(width(3), 2);
        assert_eq!(width(4), 2);
        assert_eq!(width(5), 3);
        assert_eq!(width(8), 3);
        assert_eq!(width(9), 4);
    }

    #[test]
    fn a_single_variable_is_satisfiable_in_each_choice() {
        let mut s = solver(&[3]);
        for choice in 0..3 {
            let lit = s.lit(VarId::from_index(0), choice);
            assert!(s.is_sat(&lit), "choice {choice} should be reachable");
        }
    }

    #[test]
    fn choices_of_one_variable_are_mutually_exclusive() {
        let mut s = solver(&[3]);
        let a = s.lit(VarId::from_index(0), 0);
        let b = s.lit(VarId::from_index(0), 1);
        let both = s.and(&a, &b);
        assert!(!s.is_sat(&both), "one variable cannot hold two values");
    }

    #[test]
    fn domain_constraint_rules_out_the_spare_pattern() {
        // 3 choices needs 2 bits; the pattern `11` (value 3) must be unreachable.
        let mut s = solver(&[3]);
        let spare = s.encode(0, 2, 3);
        assert!(!s.is_sat(&spare), "value 3 does not exist for a 3-choice var");
    }

    #[test]
    fn independent_variables_combine_freely() {
        let mut s = solver(&[2, 4]);
        let x = s.lit(VarId::from_index(0), 1);
        let y = s.lit(VarId::from_index(1), 2);
        let both = s.and(&x, &y);
        assert!(s.is_sat(&both));
    }

    #[test]
    fn assume_narrows_the_space() {
        let mut s = solver(&[2, 2]);
        let x0 = s.lit(VarId::from_index(0), 0);
        s.assume(&x0);

        let x1 = s.lit(VarId::from_index(0), 1);
        assert!(!s.is_sat(&x1), "x==1 was assumed away");

        let y1 = s.lit(VarId::from_index(1), 1);
        assert!(s.is_sat(&y1), "the other variable is still free");
    }

    #[test]
    fn top_is_sat_and_bottom_is_not() {
        let mut s = BddSolver::new();
        let t = s.top();
        let f = s.bottom();
        assert!(s.is_sat(&t));
        assert!(!s.is_sat(&f));
    }

    #[test]
    fn not_of_a_full_partition_is_unsat() {
        let mut s = solver(&[3]);
        let v = VarId::from_index(0);
        let any = {
            let a = s.lit(v, 0);
            let b = s.lit(v, 1);
            let c = s.lit(v, 2);
            let ab = s.or(&a, &b);
            s.or(&ab, &c)
        };
        let none = s.not(&any);
        assert!(!s.is_sat(&none), "the variable must take some value");
    }
}
