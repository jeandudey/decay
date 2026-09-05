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
            Repo,
            Source,
            is_full_sha, //
        },
        git_cache::{self, GitCache},
        wrapdb::{self, WrapFile, WrapSource},
    },
    eyre::{
        Context,
        bail,
        eyre, //
    },
    serde::{Deserialize, Serialize},
    std::{fs, path::Path},
    tracing::info,
    url::Url,
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
    /// The wrapdb commit `{name}_{version}` resolved to — pins
    /// `patch_directory`'s content the same way `source_hash`/`git_rev`
    /// pins the rest, since that tag is not guaranteed to still point where
    /// it did (wrapdb has force-moved one before).
    wrapdb_rev: String,
    #[serde(default)]
    patch_directory: Option<String>,
    /// Exactly one of these two field groups is populated, depending on
    /// whether the wrap turned out to be `[wrap-file]` or `[wrap-git]`.
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    source_filename: Option<String>,
    #[serde(default)]
    source_hash: Option<String>,
    #[serde(default)]
    git_url: Option<String>,
    /// Always a full commit hash by the time it's written here — resolved
    /// once, the same as a `[[project]]`'s own `rev`.
    #[serde(default)]
    git_rev: Option<String>,
}

impl LockedWrap {
    fn new(name: &str, pin: Option<&str>, version: &str, file: &WrapFile) -> Self {
        let (source_url, source_filename, source_hash, git_url, git_rev) = match &file.source {
            WrapSource::Archive { url, filename, hash } => {
                (Some(url.clone()), Some(filename.clone()), Some(hash.clone()), None, None)
            }
            WrapSource::Git { url, revision } => (None, None, None, Some(url.clone()), Some(revision.clone())),
        };
        LockedWrap {
            name: name.to_owned(),
            pin: pin.map(str::to_owned),
            version: version.to_owned(),
            wrapdb_rev: file.wrapdb_rev.clone(),
            patch_directory: file.patch_directory.clone(),
            source_url,
            source_filename,
            source_hash,
            git_url,
            git_rev,
        }
    }

    fn to_wrap_file(&self) -> eyre::Result<WrapFile> {
        let source = match (&self.source_url, &self.git_url) {
            (Some(url), None) => WrapSource::Archive {
                url: url.clone(),
                filename: self
                    .source_filename
                    .clone()
                    .ok_or_else(|| eyre!("locked wrap `{}` has a `source_url` but no `source_filename`", self.name))?,
                hash: self
                    .source_hash
                    .clone()
                    .ok_or_else(|| eyre!("locked wrap `{}` has a `source_url` but no `source_hash`", self.name))?,
            },
            (None, Some(url)) => WrapSource::Git {
                url: url.clone(),
                revision: self
                    .git_rev
                    .clone()
                    .ok_or_else(|| eyre!("locked wrap `{}` has a `git_url` but no `git_rev`", self.name))?,
            },
            _ => bail!(
                "locked wrap `{}` has neither a `source_url` nor a `git_url` (or has both) — \
                 decay.lock may be hand-edited or corrupt",
                self.name
            ),
        };
        Ok(WrapFile {
            source,
            patch_directory: self.patch_directory.clone(),
            wrapdb_rev: self.wrapdb_rev.clone(),
        })
    }
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
                    Some(locked) => (locked.version.clone(), locked.to_wrap_file()?),
                    None => {
                        let resolved_version = match version {
                            Some(v) => v.clone(),
                            None => wrapdb::latest_version(git_cache, name)?,
                        };
                        let mut file = wrapdb::fetch(git_cache, name, &resolved_version).wrap_err_with(|| {
                            format!("Failed to fetch the `{name}` wrap at `{resolved_version}`")
                        })?;
                        // A `[wrap-git]` `revision` may name a branch or tag,
                        // same as a `[[project]]`'s own `rev` — pin it to the
                        // commit it currently names, once, right here.
                        if let WrapSource::Git { url, revision } = &mut file.source {
                            if !is_full_sha(revision) {
                                let repo = Repo(Url::parse(url).wrap_err_with(|| {
                                    format!("`{url}` (the `[wrap-git]` url for `{name}`) is not a URL")
                                })?);
                                *revision = git_cache::resolve_rev(&repo, revision).wrap_err_with(|| {
                                    format!("Failed to resolve `{revision}` for the `{name}` wrap")
                                })?;
                            }
                        }
                        info!(name, version = resolved_version, "locked");
                        lock.wraps.retain(|w| &w.name != name);
                        lock.wraps.push(LockedWrap::new(name, version.as_deref(), &resolved_version, &file));
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
        let file = WrapFile {
            source: WrapSource::Archive {
                url: "https://example.test/zlib-1.3.1.tar.gz".to_owned(),
                filename: "zlib-1.3.1.tar.gz".to_owned(),
                hash: "deadbeef".to_owned(),
            },
            patch_directory: None,
            wrapdb_rev: "f".repeat(40),
        };
        save(&path, &LockFile {
            projects: vec![],
            wraps: vec![LockedWrap::new("zlib", None, "1.3.1-1", &file)],
        })
        .unwrap();

        let resolved = resolve(&path, &[wrap_project("zlib", None)], &tmp_git_cache()).unwrap();

        match &resolved[0] {
            Resolved::Wrap { version, file } => {
                assert_eq!(version, "1.3.1-1");
                assert_eq!(
                    file.source,
                    WrapSource::Archive {
                        url: "https://example.test/zlib-1.3.1.tar.gz".to_owned(),
                        filename: "zlib-1.3.1.tar.gz".to_owned(),
                        hash: "deadbeef".to_owned(),
                    }
                );
                assert_eq!(file.wrapdb_rev, "f".repeat(40));
            }
            Resolved::Git { .. } => panic!("expected a wrap"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_locked_git_wrap_is_reused_without_re_resolving() {
        let path = tmp_lock_path("reuse-wrap-git");
        let resolved_sha = "c".repeat(40);
        let file = WrapFile {
            source: WrapSource::Git {
                url: "https://example.test/x.git".to_owned(),
                revision: resolved_sha.clone(),
            },
            patch_directory: None,
            wrapdb_rev: "f".repeat(40),
        };
        save(&path, &LockFile {
            projects: vec![],
            wraps: vec![LockedWrap::new("x", None, "1.0-1", &file)],
        })
        .unwrap();

        // No real `x.git` exists at this URL, so if this reached the network
        // path (re-resolving `revision`) it would fail rather than quietly
        // reuse the pin.
        let resolved = resolve(&path, &[wrap_project("x", None)], &tmp_git_cache()).unwrap();

        match &resolved[0] {
            Resolved::Wrap { file, .. } => assert_eq!(file.source, WrapSource::Git {
                url: "https://example.test/x.git".to_owned(),
                revision: resolved_sha,
            }),
            Resolved::Git { .. } => panic!("expected a wrap"),
        }
        let _ = fs::remove_file(&path);
    }
}
