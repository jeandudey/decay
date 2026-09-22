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
    /// `meson.override_find_program(name, program)` calls: a sibling project
    /// resolving `find_program(name)` finds this project's own compiled or
    /// generated tool instead of needing a `decay.toml` `[programs]` entry
    /// (glib's `gobject/meson.build` builds `glib-mkenums`/`glib-genmarshal`
    /// from its own `.in` templates and registers them this way, precisely
    /// so a project that bootstraps against it — anything using
    /// `gnome.mkenums()`/`gnome.genmarshal()` — never needs a system copy).
    pub programs_provided: Vec<(String, TargetId)>,
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

    /// Rename a target whose auto-assigned name collides with `reserved` —
    /// the label the project's own fetch (`Project::repo_target`) or wrapdb
    /// overlay (`Project::wrapdb_target`) is about to use. Those names are
    /// only known once the project's `Origin` resolves, which happens after
    /// every real target already went through [`Self::add`] and claimed
    /// whatever name it wanted — so a project whose own `library()`/etc.
    /// happens to share the project's name (fribidi's `library('fribidi',
    /// ...)`, same as `project('fribidi', ...)`) would otherwise collide
    /// with the fetch silently until the backend tries to emit both under
    /// one label. Picks a fresh name through the same uniquification
    /// [`Self::add`] used, so the result is exactly what a second real
    /// target named `reserved` would have gotten.
    pub fn avoid_name_collision(&mut self, reserved: &str) {
        let Some(id) = self
            .targets
            .iter()
            .find(|t| t.name == reserved)
            .map(|t| t.id)
        else {
            return;
        };
        let label = self.target(id).label.clone();
        let name = self.unique_name(&label);
        self.target_mut(id).name = name;
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
