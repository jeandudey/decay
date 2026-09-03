use crate::{
    oracle::Oracle,
    val::Value, //
};

pub struct Symbolic<O: Oracle> {
    concrete: O,
}

impl<O: Oracle> Symbolic<O> {
    pub fn new(concrete: O) -> Self {
        Self { concrete }
    }
}

impl<O: Oracle> Oracle for Symbolic<O> {
    fn get_option(&self, name: &str) -> Value {
        match self.concrete.get_option(name) {
            Value::Unset => (),
            v => return v,
        }

        Value::Unset
    }

    fn machine_system(&self, machine: crate::obj::Machine) -> Value {
        self.concrete.machine_system(machine)
    }
}
