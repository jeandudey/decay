use crate::{oracle::Oracle, val::Const};

#[derive(Debug)]
pub struct Concrete {}

impl Oracle for Concrete {
    fn get_option(&self, name: &str) -> Const {
        Const::Unset
    }
}
