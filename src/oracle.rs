use {
    crate::config::{
        Config,
        Machine,
        OptionValue,
        Project, //
    },
    decay_meson_eval::{
        obj,
        oracle::{
            Oracle,
            Pinned, //
        },
    },
    std::rc::Rc,
};

/// Answers from the importer's own configuration.
///
/// Everything it declines to answer is left open, which is the default: a
/// project's options should stay options in the generated build.
pub struct ConfigOracle<'a> {
    project: &'a Project,
    config: &'a Config,
}

impl<'a> ConfigOracle<'a> {
    pub fn new(config: &'a Config, project: &'a Project) -> Self {
        Self { project, config }
    }

    fn machine_config(&self, machine: obj::Machine) -> &'a Machine {
        match machine {
            obj::Machine::Build => &self.project.build_machine,
            // Meson treats the target machine as the host one unless the
            // project is cross-compiling a compiler, which this importer does
            // not model.
            obj::Machine::Host | obj::Machine::Target => &self.project.host_machine,
        }
    }
}

impl Oracle for ConfigOracle<'_> {
    fn option(&self, name: &str) -> Option<Pinned> {
        Some(match self.project.options.get(name)? {
            OptionValue::Bool(v) => Pinned::Bool(*v),
            OptionValue::Int(v) => Pinned::Int(*v),
            OptionValue::String(v) => Pinned::Str(Rc::from(v.as_str())),
            OptionValue::List(v) => {
                Pinned::List(v.iter().map(|s| Rc::from(s.as_str())).collect())
            }
        })
    }

    fn machine(&self, machine: obj::Machine, property: &str) -> Option<String> {
        self.machine_config(machine)
            .property(property)
            .map(str::to_owned)
    }

    fn systems(&self) -> Vec<String> {
        self.config.systems.keys().cloned().collect()
    }

    fn compilers(&self) -> Vec<String> {
        if self.config.compilers.is_empty() {
            return ["gcc", "clang", "msvc"].map(str::to_owned).to_vec();
        }
        self.config.compilers.keys().cloned().collect()
    }
}
