use {
    decay_build_ir::{Graph, Kind, Source},
    std::collections::BTreeMap,
};

/// What earlier projects are known to provide, keyed by the name
/// `dependency()` looks up.
///
/// Built up as projects execute, in the order `decay.toml` lists them: after
/// a project runs, whatever it makes available under `import('pkgconfig')`
/// or a `.pc`-producing `configure_file()` is recorded here, so a project
/// listed after it can resolve a `dependency()` on it without `decay.toml`
/// having to repeat what importing that project already determined. A
/// project can only resolve against one listed before it — the reason to
/// list libraries ahead of what uses them, same as a real dependency graph.
#[derive(Debug, Clone, Default)]
pub struct Packages {
    by_name: BTreeMap<String, Package>,
}

#[derive(Debug, Clone)]
pub struct Package {
    /// Build-file label of the target carrying its usage requirements, when
    /// it is a linkable library and not just data.
    pub target: Option<String>,
    /// The `.c` sources a `declare_dependency(sources: …)` copylib
    /// contributes, as package-qualified Starlark source refs — a consumer
    /// splices these straight into its own `srcs` (see `decay_buck2`). Empty
    /// for an ordinary library provider.
    pub sources: Vec<String>,
    pub variables: Vec<(String, String)>,
}

impl Packages {
    pub fn get(&self, name: &str) -> Option<&Package> {
        self.by_name.get(name)
    }

    /// Every provided name that names a linkable target, as `(name, label)`.
    pub fn targets(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.by_name
            .iter()
            .filter_map(|(name, pkg)| Some((name.clone(), pkg.target.clone()?)))
    }

    /// Every provided name that contributes copylib `.c` sources, as
    /// `(name, refs)`.
    pub fn source_groups(&self) -> impl Iterator<Item = (String, Vec<String>)> + '_ {
        self.by_name.iter().filter_map(|(name, pkg)| {
            (!pkg.sources.is_empty()).then(|| (name.clone(), pkg.sources.clone()))
        })
    }

    /// Record what one project provides, once it has finished executing.
    ///
    /// `package` is the build-file package it was written to, e.g.
    /// `third-party/meson/libepoxy`, which is how its targets are named from
    /// anywhere else.
    pub fn register(&mut self, package: &str, graph: &Graph) {
        for provide in &graph.provides {
            let iface = provide.target.map(|id| graph.target(id));
            let target = iface.map(|t| format!("//{package}:{}", t.name));
            // Only a `declare_dependency()` copylib (an interface target)
            // contributes sources for a consumer to compile; a real library
            // provider compiles its own `srcs` and a consumer just links it.
            let sources = iface
                .filter(|t| matches!(t.kind, Kind::Interface))
                .map(|t| {
                    t.attrs
                        .srcs
                        .iter()
                        .map(|s| match &s.value {
                            Source::File(path) => {
                                format!("//{package}:{}.git[{}]", graph.project.name, path.display())
                            }
                            Source::Generated(id) => {
                                format!("//{package}:{}", graph.target(*id).name)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.by_name.insert(
                provide.name.clone(),
                Package {
                    target,
                    sources,
                    variables: provide.variables.clone(),
                },
            );
        }
    }
}
