use {
    crate::{
        Attrs,
        Install,
        Kind,
        Package,
        Project,
        Target,
        TargetId,
        Test, //
    },
    decay_meson_logic::{
        Pc,
        Var, //
    },
    std::{
        collections::HashMap,
        path::{
            Path,
            PathBuf, //
        },
    },
};

/// The whole build, in backend-neutral form.
#[derive(Debug, Default)]
pub struct Graph {
    pub project: Project,
    /// The configuration surface: every variable the executor could not pin
    /// down. A backend turns these into whatever knobs it has.
    pub options: Vec<Var>,
    pub targets: Vec<Target>,
    pub tests: Vec<Test>,
    /// Files the project installs outside of a target's own output.
    pub installs: Vec<Install>,
    /// `pkg-config` modules the project makes available to others.
    pub provides: Vec<Package>,
    /// Names already handed out, so generated names stay unique.
    used_names: HashMap<String, u32>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn target(&self, id: TargetId) -> &Target {
        &self.targets[id.0 as usize]
    }

    pub fn target_mut(&mut self, id: TargetId) -> &mut Target {
        &mut self.targets[id.0 as usize]
    }

    pub fn add(&mut self, label: &str, package: &Path, cond: Pc, kind: Kind) -> TargetId {
        let id = TargetId(self.targets.len() as u32);
        let name = self.unique_name(label);
        self.targets.push(Target {
            id,
            name,
            label: label.to_owned(),
            package: package.to_path_buf(),
            cond,
            kind,
            attrs: Attrs::default(),
        });
        id
    }

    /// Targets that live in `package`, in declaration order.
    pub fn in_package<'a>(&'a self, package: &'a Path) -> impl Iterator<Item = &'a Target> + 'a {
        self.targets.iter().filter(move |t| t.package == package)
    }

    pub fn packages(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for t in &self.targets {
            if !out.contains(&t.package) {
                out.push(t.package.clone());
            }
        }
        out
    }

    /// A name that is safe to use as a build-file label and unique in the
    /// graph. Meson lets two targets in different directories share a name;
    /// most backends do not.
    fn unique_name(&mut self, label: &str) -> String {
        let base: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let base = if base.is_empty() {
            "unnamed".to_owned()
        } else {
            base
        };
        let n = self.used_names.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 {
            base
        } else {
            format!("{base}-{}", *n)
        }
    }
}
