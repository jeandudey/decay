use {
    crate::config::{Machine, OptionValue},
    decay_meson_eval::{obj, oracle::Oracle, val::Value},
    std::collections::BTreeMap,
    tracing::trace,
};

pub struct Concrete<'a> {
    options: &'a BTreeMap<String, OptionValue>,
    host_machine: &'a Machine,
}

impl<'a> Concrete<'a> {
    pub fn new(options: &'a BTreeMap<String, OptionValue>, host_machine: &'a Machine) -> Self {
        Self {
            options,
            host_machine,
        }
    }
}

impl<'a> Oracle for Concrete<'a> {
    fn get_option(&self, name: &str) -> Value {
        todo!()
        //match self.options.get(name) {
        //    Some(OptionValue::Bool(v)) => Value::Bool(*v),
        //    Some(OptionValue::Int(v)) => Value::Int(*v),
        //    Some(OptionValue::String(v)) => Value::Str(v.clone()),
        //    Some(OptionValue::List(v)) => {
        //        Value::List(v.iter().map(|v| Value::Str(v.clone())).collect())
        //    }
        //    None => {
        //        trace!(name, "no default option");
        //        Value::Unset
        //    }
        //}
    }

    fn machine_system(&self, machine: obj::Machine) -> Value {
        todo!()
        //match machine {
        //    obj::Machine::Host => self
        //        .host_machine
        //        .system
        //        .as_ref()
        //        .map(|v| Value::Str(v.clone()))
        //        .unwrap_or(Value::Unset),
        //}
    }
}
