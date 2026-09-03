//! Ordering the projects in `decay.toml` into waves that can run in parallel.
//!
//! A project's `depends` names the siblings that must finish before it starts,
//! because a `dependency()` on one of them only resolves once it has been
//! executed and registered (see [`crate::packages`]). Everything else about the
//! run stays sequential-looking: waves are produced in order, and within a wave
//! project indices keep their `decay.toml` order, so what accumulates into
//! `Packages` is a pure function of the project set, not of timing.

use {
    crate::config::Project,
    eyre::{
        bail,
        eyre, //
    },
    std::collections::HashMap,
};

/// Projects grouped into dependency waves.
#[derive(Debug)]
pub struct Schedule {
    /// Each inner `Vec` is one wave of indices into the project list. Every
    /// index appears exactly once; a wave's dependencies are all satisfied by
    /// earlier waves; indices within a wave are ascending.
    pub waves: Vec<Vec<usize>>,
}

/// Plan the run for `projects`.
///
/// With no `depends` anywhere the plan is the historical one: one project per
/// wave, in file order. Otherwise waves come from the `depends` DAG. Errors on
/// an unknown or self `depends`, a duplicated project name, or a cycle.
pub fn plan(projects: &[Project]) -> eyre::Result<Schedule> {
    let names: Vec<String> = projects.iter().map(|p| p.repo.short_name()).collect();

    // No `depends` at all: keep the strict sequential fold, unchanged.
    if projects.iter().all(|p| p.depends.is_empty()) {
        return Ok(Schedule {
            waves: (0..projects.len()).map(|i| vec![i]).collect(),
        });
    }

    let mut index_by_name: HashMap<&str, usize> = HashMap::with_capacity(names.len());
    for (i, name) in names.iter().enumerate() {
        if let Some(prev) = index_by_name.insert(name, i) {
            bail!(
                "projects {prev} and {i} in `decay.toml` are both called `{name}`; \
                 `depends` cannot tell them apart"
            );
        }
    }

    // deps[i] = the indices project i must follow, deduplicated.
    let mut deps: Vec<Vec<usize>> = Vec::with_capacity(projects.len());
    for (i, project) in projects.iter().enumerate() {
        let mut this: Vec<usize> = Vec::new();
        for name in &project.depends {
            let &j = index_by_name.get(name.as_str()).ok_or_else(|| {
                eyre!(
                    "`{}` depends on `{name}`, which is not a project in `decay.toml`",
                    names[i]
                )
            })?;
            if j == i {
                bail!("`{}` depends on itself", names[i]);
            }
            if !this.contains(&j) {
                this.push(j);
            }
        }
        deps.push(this);
    }

    // Longest-path layering over the DAG, Kahn-style. `pending` counts a node's
    // unplaced dependencies; a node drops into the next wave once that hits 0.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); projects.len()];
    let mut pending: Vec<usize> = vec![0; projects.len()];
    for (i, this) in deps.iter().enumerate() {
        pending[i] = this.len();
        for &j in this {
            dependents[j].push(i);
        }
    }

    let mut placed = vec![false; projects.len()];
    let mut waves: Vec<Vec<usize>> = Vec::new();
    let mut frontier: Vec<usize> = (0..projects.len()).filter(|&i| pending[i] == 0).collect();
    let mut done = 0;

    while !frontier.is_empty() {
        frontier.sort_unstable();
        for &i in &frontier {
            placed[i] = true;
        }
        done += frontier.len();

        let mut next: Vec<usize> = Vec::new();
        for &i in &frontier {
            for &d in &dependents[i] {
                pending[d] -= 1;
                if pending[d] == 0 {
                    next.push(d);
                }
            }
        }
        waves.push(std::mem::take(&mut frontier));
        frontier = next;
    }

    if done != projects.len() {
        let cycle: Vec<&str> = placed
            .iter()
            .enumerate()
            .filter(|&(_, &p)| !p)
            .map(|(i, _)| names[i].as_str())
            .collect();
        bail!(
            "`depends` in `decay.toml` has a cycle among: {}",
            cycle.join(", ")
        );
    }

    Ok(Schedule { waves })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::config::{
            Machine,
            Repo, //
        },
        url::Url,
    };

    fn project(name: &str, depends: &[&str]) -> Project {
        Project {
            repo: Repo(Url::parse(&format!("https://example.test/{name}.git")).unwrap()),
            rev: "0".repeat(40),
            options: Default::default(),
            host_machine: Machine::default(),
            build_machine: Machine::default(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_depends_is_one_project_per_wave() {
        let projects = [project("a", &[]), project("b", &[]), project("c", &[])];
        let schedule = plan(&projects).unwrap();
        assert_eq!(schedule.waves, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn independent_projects_share_a_wave() {
        // c depends on a and b; a and b are independent.
        let projects = [
            project("a", &[]),
            project("b", &[]),
            project("c", &["a", "b"]),
        ];
        let schedule = plan(&projects).unwrap();
        assert_eq!(schedule.waves, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn wave_is_one_past_deepest_dependency() {
        // a -> b -> d, and c -> d, so d is wave 0, {b,c} wave 1, a wave 2.
        let projects = [
            project("a", &["b"]),
            project("b", &["d"]),
            project("c", &["d"]),
            project("d", &[]),
        ];
        let schedule = plan(&projects).unwrap();
        assert_eq!(schedule.waves, vec![vec![3], vec![1, 2], vec![0]]);
    }

    #[test]
    fn indices_within_a_wave_are_ascending() {
        let projects = [
            project("late", &["dep"]),
            project("dep", &[]),
            project("early", &[]),
        ];
        let schedule = plan(&projects).unwrap();
        assert_eq!(schedule.waves, vec![vec![1, 2], vec![0]]);
    }

    #[test]
    fn unknown_depends_is_an_error() {
        let projects = [project("a", &["ghost"])];
        let err = plan(&projects).unwrap_err().to_string();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn self_depends_is_an_error() {
        let projects = [project("a", &["a"])];
        assert!(plan(&projects).unwrap_err().to_string().contains("itself"));
    }

    #[test]
    fn cycle_is_an_error() {
        let projects = [project("a", &["b"]), project("b", &["a"])];
        let err = plan(&projects).unwrap_err().to_string();
        assert!(err.contains("cycle"), "{err}");
    }

    #[test]
    fn duplicate_name_is_an_error_once_depends_used() {
        let projects = [
            project("dup", &[]),
            project("dup", &[]),
            project("x", &["dup"]),
        ];
        assert!(plan(&projects).unwrap_err().to_string().contains("both called"));
    }

    #[test]
    fn repeated_depends_entry_is_harmless() {
        let projects = [project("a", &[]), project("b", &["a", "a"])];
        let schedule = plan(&projects).unwrap();
        assert_eq!(schedule.waves, vec![vec![0], vec![1]]);
    }
}
