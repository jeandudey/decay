mod z3;

use std::fmt::Debug;

pub use z3::Z3Solver;

pub trait Solver {
    type Term: Clone + Debug;

    /// Always `true` term.
    fn top(&mut self) -> Self::Term;

    /// Always `false` term.
    fn bottom(&mut self) -> Self::Term;

    fn not(&mut self, t: &Self::Term) -> Self::Term;

    fn and(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term;

    fn or(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term;

    fn is_sat(&mut self, t: &Self::Term) -> bool;
}
