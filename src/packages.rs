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
    /// The `.pc` `Requires:` — other provided names a consumer of this one
    /// also needs on its include/link path.
    pub requires: Vec<String>,
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

    /// Every provided name that names a linkable target, as `(name, labels)`:
    /// the target itself, followed by the targets of its `.pc` `Requires:`
    /// (transitively), so a consumer resolving `dependency('name')` gets the
    /// same include/link closure `pkg-config --cflags name` would give it.
    pub fn targets(&self) -> impl Iterator<Item = (String, Vec<String>)> + '_ {
        self.by_name.iter().filter_map(|(name, pkg)| {
            let mut labels = vec![pkg.target.clone()?];
            let mut queue = pkg.requires.clone();
            let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            while let Some(req) = queue.pop() {
                if !seen.insert(req.clone()) {
                    continue;
                }
                if let Some(dep) = self.by_name.get(&req) {
                    if let Some(label) = &dep.target
                        && !labels.contains(label)
                    {
                        labels.push(label.clone());
                    }
                    queue.extend(dep.requires.clone());
                }
            }
            Some((name.clone(), labels))
        })
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
                    requires: provide.requires.clone(),
                    sources,
                    variables: provide.variables.clone(),
                },
            );
        }
    }
}
