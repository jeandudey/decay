mod z3;

use {
    crate::arena::VarId,
    std::fmt::Debug, //
};

pub use z3::Z3Solver;

/// The satisfiability backend behind [`crate::Logic`].
pub trait Solver {
    type Term: Clone + Debug;

    /// Introduce a variable with `n_choices` possible values.
    fn declare(&mut self, var: VarId, n_choices: u32);

    /// The term for "`var` took its `choice`th value".
    fn lit(&mut self, var: VarId, choice: u32) -> Self::Term;

    /// Always `true` term.
    fn top(&mut self) -> Self::Term;

    /// Always `false` term.
    fn bottom(&mut self) -> Self::Term;

    fn not(&mut self, t: &Self::Term) -> Self::Term;

    fn and(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term;

    fn or(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term;

    /// Constrain the whole configuration space, ruling out every assignment
    /// that falsifies `t`.
    fn assume(&mut self, t: &Self::Term);

    fn is_sat(&mut self, t: &Self::Term) -> bool;
}
