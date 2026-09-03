use crate::{obj::Machine, val::Value};

mod symbolic;

pub use symbolic::Symbolic;

pub trait Oracle {
    fn get_option(&self, name: &str) -> Value;

    fn machine_system(&self, machine: Machine) -> Value;
}
