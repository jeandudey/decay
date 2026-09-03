use std::collections::HashMap;

use crate::{Arena, Pc, Solver};

#[derive(Debug)]
pub struct Logic<S: Solver> {
    solver: S,
    arena: Arena,
    terms: HashMap<Pc, S::Term>,
    cache: HashMap<Pc, bool>,
}

impl<S: Solver> Logic<S> {
    pub fn new(solver: S) -> Self {
        Self {
            solver,
            arena: Arena::new(),
            terms: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    pub fn and(&mut self, a: Pc, b: Pc) -> Pc {
        self.arena.and(a, b)
    }

    pub fn or(&mut self, a: Pc, b: Pc) -> Pc {
        self.arena.or(a, b)
    }

    pub fn is_sat(&mut self, v: Pc) -> bool {
        todo!()
    }
}
