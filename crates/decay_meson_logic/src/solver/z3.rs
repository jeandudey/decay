use z3::ast::Bool;

use crate::Solver;

#[derive(Debug)]
pub struct Z3Solver {}

impl Solver for Z3Solver {
    type Term = Bool;

    fn top(&mut self) -> Self::Term {
        todo!()
    }

    fn bottom(&mut self) -> Self::Term {
        todo!()
    }

    fn not(&mut self, t: &Self::Term) -> Self::Term {
        todo!()
    }

    fn and(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term {
        todo!()
    }

    fn or(&mut self, a: &Self::Term, b: &Self::Term) -> Self::Term {
        todo!()
    }

    fn is_sat(&mut self, t: &Self::Term) -> bool {
        todo!()
    }
}
