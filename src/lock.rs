//! `decay.lock`: pins whatever `decay.toml` left to resolve against a moving
//! target — a git `rev` that names a branch or tag rather than a commit, and
//! a wrap's wrapdb version when `decay.toml` does not pin one itself — so a
//! second run reproduces the first one instead of picking up whatever
//! upstream or wrapdb happens to have moved on to.
//!
//! Resolved once per distinct `(repo, rev)` or `(wrap, version)` pin and then
//! written back; deleting `decay.lock`, or changing what it was resolved
//! from, is what makes it re-resolve, the same relationship `Cargo.lock` has
//! to a version requirement in `Cargo.toml`.

use {
    crate::{
        config::{
            Project,
            Source,
            is_full_sha, //
        },
        git_cache::{self, GitCache},
        wrapdb::{self, WrapFile},
    },
    eyre::Context,
    serde::{Deserialize, Serialize},
    std::{fs, path::Path},
    tracing::info,
};

#[derive(Debug, Default, Serialize, Deserialize)]
struct LockFile {
    #[serde(rename = "project", default)]
    projects: Vec<LockedGit>,
    #[serde(rename = "wrap", default)]
    wraps: Vec<LockedWrap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockedGit {
    repo: String,
    /// The `decay.toml` `rev` this pin was resolved from, so a changed `rev`
    /// re-resolves instead of reusing a pin that no longer applies.
    rev: String,
    resolved: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockedWrap {
    name: String,
    /// The `decay.toml` `version`, if it pinned one — `None` means "latest",
    /// and the lock keeps whatever that resolved to until `decay.lock` is
    /// deleted or edited by hand.
    pin: Option<String>,
    version: String,
    source_url: String,
    source_filename: String,
    source_hash: String,
    #[serde(default)]
    patch_directory: Option<String>,
}

/// What a project resolved to, ready for [`crate::execute`] to fetch: a git
/// project's `rev` turned into a full commit hash, and a wrap's version and
/// wrap-file contents as wrapdb answered them.
pub enum Resolved {
    Git { rev: String },
    Wrap { version: String, file: WrapFile },
}

/// Resolve every project in `projects`, consulting and then updating the lock
/// file at `path`. One [`Resolved`] per entry of `projects`, in the same
/// order.
pub fn resolve(path: &Path, projects: &[Project], git_cache: &GitCache) -> eyre::Result<Vec<Resolved>> {
    let mut lock = load(path)?;
    let mut dirty = false;
    let mut out = Vec::with_capacity(projects.len());

    for project in projects {
        match &project.source {
            Source::Git { repo, rev } => {
                if is_full_sha(rev) {
                    out.push(Resolved::Git { rev: rev.clone() });
                    continue;
                }

                let repo_url = repo.0.to_string();
                let resolved = match lock.projects.iter().find(|p| p.repo == repo_url && p.rev == *rev) {
                    Some(locked) => locked.resolved.clone(),
                    None => {
                        let resolved = git_cache::resolve_rev(repo, rev).wrap_err_with(|| {
                            format!("Failed to resolve `{rev}` for `{}`", repo.short_name())
                        })?;
                        info!(repo = %repo_url, rev, resolved, "locked");
                        lock.projects.retain(|p| p.repo != repo_url);
                        lock.projects.push(LockedGit {
                            repo: repo_url,
                            rev: rev.clone(),
                            resolved: resolved.clone(),
                        });
                        dirty = true;
                        resolved
                    }
                };
                out.push(Resolved::Git { rev: resolved });
            }

            Source::Wrap { name, version } => {
                let existing = lock
                    .wraps
                    .iter()
                    .find(|w| &w.name == name && w.pin.as_deref() == version.as_deref());
                let (resolved_version, file) = match existing {
                    Some(locked) => (locked.version.clone(), WrapFile {
                        source_url: locked.source_url.clone(),
                        source_filename: locked.source_filename.clone(),
                        source_hash: locked.source_hash.clone(),
                        patch_directory: locked.patch_directory.clone(),
                    }),
                    None => {
                        let resolved_version = match version {
                            Some(v) => v.clone(),
                            None => wrapdb::latest_version(git_cache, name)?,
                        };
                        let file = wrapdb::fetch(git_cache, name, &resolved_version).wrap_err_with(|| {
                            format!("Failed to fetch the `{name}` wrap at `{resolved_version}`")
                        })?;
                        info!(name, version = resolved_version, "locked");
                        lock.wraps.retain(|w| &w.name != name);
                        lock.wraps.push(LockedWrap {
                            name: name.clone(),
                            pin: version.clone(),
                            version: resolved_version.clone(),
                            source_url: file.source_url.clone(),
                            source_filename: file.source_filename.clone(),
                            source_hash: file.source_hash.clone(),
                            patch_directory: file.patch_directory.clone(),
                        });
                        dirty = true;
                        (resolved_version, file)
                    }
                };
                out.push(Resolved::Wrap { version: resolved_version, file });
            }
        }
    }

    if dirty {
        save(path, &lock)?;
    }
    Ok(out)
}

fn load(path: &Path) -> eyre::Result<LockFile> {
    if !path.is_file() {
        return Ok(LockFile::default());
    }
    let text =
        fs::read_to_string(path).wrap_err_with(|| format!("Failed to read {}", path.display()))?;
    toml::from_str(&text).wrap_err_with(|| format!("Failed to parse {}", path.display()))
}

fn save(path: &Path, lock: &LockFile) -> eyre::Result<()> {
    let text = toml::to_string_pretty(lock).wrap_err("Failed to serialize decay.lock")?;
    fs::write(path, text).wrap_err_with(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::config::{
            Machine,
            Repo, //
        },
        std::{
            path::PathBuf,
            process,
            sync::atomic::{
                AtomicUsize,
                Ordering, //
            },
        },
        url::Url,
    };

    fn tmp_lock_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("decay-lock-test-{tag}-{}-{n}.toml", process::id()))
    }

    /// A cache these tests never actually touch — every test here reuses an
    /// existing lock entry rather than resolving one, so nothing here reaches
    /// the network.
    fn tmp_git_cache() -> GitCache {
        GitCache::new(std::env::temp_dir().join(format!("decay-lock-test-cache-{}", process::id())))
    }

    fn git_project(repo: &str, rev: &str) -> Project {
        Project {
            source: Source::Git { repo: Repo(Url::parse(repo).unwrap()), rev: rev.to_owned() },
            options: Default::default(),
            host_machine: Machine::default(),
            build_machine: Machine::default(),
            depends: Vec::new(),
        }
    }

    fn wrap_project(name: &str, version: Option<&str>) -> Project {
        Project {
            source: Source::Wrap { name: name.to_owned(), version: version.map(str::to_owned) },
            options: Default::default(),
            host_machine: Machine::default(),
            build_machine: Machine::default(),
            depends: Vec::new(),
        }
    }

    #[test]
    fn a_full_sha_resolves_without_touching_the_lock_file() {
        let path = tmp_lock_path("full-sha");
        let sha = "a".repeat(40);
        let projects = [git_project("https://example.test/x.git", &sha)];

        let resolved = resolve(&path, &projects, &tmp_git_cache()).unwrap();

        assert!(matches!(&resolved[0], Resolved::Git { rev } if *rev == sha));
        assert!(!path.exists(), "a fully-pinned rev needs no lock entry");
    }

    #[test]
    fn a_locked_branch_is_reused_without_re_resolving() {
        let path = tmp_lock_path("reuse-git");
        let repo = "https://example.test/y.git";
        let resolved_sha = "b".repeat(40);
        save(&path, &LockFile {
            projects: vec![LockedGit {
                repo: repo.to_owned(),
                rev: "main".to_owned(),
                resolved: resolved_sha.clone(),
            }],
            wraps: vec![],
        })
        .unwrap();

        // No real `main` branch exists at this URL, so if this reached the
        // network path it would fail rather than quietly resolve to
        // something else.
        let resolved = resolve(&path, &[git_project(repo, "main")], &tmp_git_cache()).unwrap();

        assert!(matches!(&resolved[0], Resolved::Git { rev } if *rev == resolved_sha));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_locked_wrap_is_reused_without_querying_wrapdb() {
        let path = tmp_lock_path("reuse-wrap");
        save(&path, &LockFile {
            projects: vec![],
            wraps: vec![LockedWrap {
                name: "zlib".to_owned(),
                pin: None,
                version: "1.3.1-1".to_owned(),
                source_url: "https://example.test/zlib-1.3.1.tar.gz".to_owned(),
                source_filename: "zlib-1.3.1.tar.gz".to_owned(),
                source_hash: "deadbeef".to_owned(),
                patch_directory: None,
            }],
        })
        .unwrap();

        let resolved = resolve(&path, &[wrap_project("zlib", None)], &tmp_git_cache()).unwrap();

        match &resolved[0] {
            Resolved::Wrap { version, file } => {
                assert_eq!(version, "1.3.1-1");
                assert_eq!(file.source_hash, "deadbeef");
            }
            Resolved::Git { .. } => panic!("expected a wrap"),
        }
        let _ = fs::remove_file(&path);
    }
}
