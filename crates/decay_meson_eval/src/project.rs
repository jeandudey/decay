use std::rc::Rc;

use crate::val::Variational;

#[derive(Debug, Default)]
pub struct Project {
    pub name: Variational<Rc<str>>,
    pub version: Variational<Rc<String>>,
}
