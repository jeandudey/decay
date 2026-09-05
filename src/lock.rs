//! `decay.lock`: pins whatever `decay.toml` left to resolve against a moving
//! target — a git `branch`, `tag`, or non-commit `rev`, and
//! a wrap's wrapdb version when `decay.toml` does not pin one itself — so a
//! second run reproduces the first one instead of picking up whatever
//! upstream or wrapdb happens to have moved on to.
//!
//! Resolved once per distinct `(repo, branch|tag|rev)` or `(wrap, version)` pin and then
//! written back; deleting `decay.lock`, or changing what it was resolved
//! from, is what makes it re-resolve, the same relationship `Cargo.lock` has
//! to a version requirement in `Cargo.toml`.

use {
    crate::{
        config::{
            GitReference,
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
    sha2::{Digest, Sha256},
    std::{
        fs,
        path::{Path, PathBuf},
    },
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
    /// The source selector is retained with its kind. A tag and a branch with
    /// the same text must never share a lock entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rev: Option<String>,
    resolved: String,
}

impl LockedGit {
    fn new(repo: String, reference: &GitReference, resolved: String) -> Self {
        let (branch, tag, rev) = match reference {
            GitReference::Branch(value) => (Some(value.clone()), None, None),
            GitReference::Tag(value) => (None, Some(value.clone()), None),
            GitReference::Rev(value) => (None, None, Some(value.clone())),
        };
        Self {
            repo,
            branch,
            tag,
            rev,
            resolved,
        }
    }

    fn matches(&self, repo: &str, reference: &GitReference) -> bool {
        self.repo == repo
            && match reference {
                GitReference::Branch(value) => self.branch.as_deref() == Some(value),
                GitReference::Tag(value) => self.tag.as_deref() == Some(value),
                GitReference::Rev(value) => self.rev.as_deref() == Some(value),
            }
    }
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
            WrapSource::Archive {
                url,
                filename,
                hash,
            } => (
                Some(url.clone()),
                Some(filename.clone()),
                Some(hash.clone()),
                None,
                None,
            ),
            WrapSource::Git { url, revision } => {
                (None, None, None, Some(url.clone()), Some(revision.clone()))
            }
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
                filename: self.source_filename.clone().ok_or_else(|| {
                    eyre!(
                        "locked wrap `{}` has a `source_url` but no `source_filename`",
                        self.name
                    )
                })?,
                hash: self.source_hash.clone().ok_or_else(|| {
                    eyre!(
                        "locked wrap `{}` has a `source_url` but no `source_hash`",
                        self.name
                    )
                })?,
            },
            (None, Some(url)) => WrapSource::Git {
                url: url.clone(),
                revision: self.git_rev.clone().ok_or_else(|| {
                    eyre!(
                        "locked wrap `{}` has a `git_url` but no `git_rev`",
                        self.name
                    )
                })?,
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
            local_overlay: None,
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
pub fn resolve(
    path: &Path,
    projects: &[Project],
    git_cache: &GitCache,
    local_wrap_dir: Option<&Path>,
) -> eyre::Result<Vec<Resolved>> {
    let mut lock = load(path)?;
    let mut dirty = false;
    let mut out = Vec::with_capacity(projects.len());

    for project in projects {
        match &project.source {
            Source::Git { repo, reference } => {
                if matches!(reference, GitReference::Rev(rev) if is_full_sha(rev)) {
                    out.push(Resolved::Git {
                        rev: reference.value().to_owned(),
                    });
                    continue;
                }

                let repo_url = repo.0.to_string();
                let resolved = match lock
                    .projects
                    .iter()
                    .find(|p| p.matches(&repo_url, reference))
                {
                    Some(locked) => locked.resolved.clone(),
                    None => {
                        let remote_ref = reference.remote_ref();
                        let resolved =
                            git_cache::resolve_rev(repo, &remote_ref).wrap_err_with(|| {
                                format!(
                                    "Failed to resolve {} `{}` for `{}`",
                                    reference.kind(),
                                    reference.value(),
                                    repo.short_name()
                                )
                            })?;
                        info!(repo = %repo_url, kind = reference.kind(), reference = reference.value(), resolved, "locked");
                        lock.projects.retain(|p| p.repo != repo_url);
                        lock.projects
                            .push(LockedGit::new(repo_url, reference, resolved.clone()));
                        dirty = true;
                        resolved
                    }
                };
                out.push(Resolved::Git { rev: resolved });
            }

            Source::Wrap { name, version } => {
                if let Some(dir) = local_wrap_dir {
                    let path = dir.join(format!("{name}.wrap"));
                    if path.is_file() {
                        if version.is_some() {
                            bail!(
                                "local wrap `{name}` at {} does not support `version`; the local .wrap file is the pin",
                                path.display()
                            );
                        }
                        let mut file = wrapdb::load_local(&path)?;
                        if let WrapSource::Git { url, revision } = &mut file.source
                            && !is_full_sha(revision)
                        {
                            let repo = Repo(Url::parse(url).wrap_err_with(|| {
                                format!("`{url}` (the `[wrap-git]` url for local wrap `{name}`) is not a URL")
                            })?);
                            *revision =
                                git_cache::resolve_rev(&repo, revision).wrap_err_with(|| {
                                    format!(
                                        "Failed to resolve `{revision}` for local wrap `{name}`"
                                    )
                                })?;
                        }
                        // The cache key includes the immutable source identity, so editing a
                        // local wrap cannot reuse a stale extracted/overlaid tree.
                        let source_version = match &file.source {
                            WrapSource::Archive { hash, .. } => &hash[..16.min(hash.len())],
                            WrapSource::Git { revision, .. } => &revision[..16.min(revision.len())],
                        };
                        let local_version = format!(
                            "local-{source_version}-{}",
                            &local_fingerprint(&path, file.local_overlay.as_deref())[..16]
                        );
                        out.push(Resolved::Wrap {
                            version: local_version,
                            file,
                        });
                        continue;
                    }
                }
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
                        let mut file = wrapdb::fetch(git_cache, name, &resolved_version)
                            .wrap_err_with(|| {
                                format!("Failed to fetch the `{name}` wrap at `{resolved_version}`")
                            })?;
                        // A `[wrap-git]` `revision` may name a branch or tag,
                        // same as a `[[project]]`'s own `rev` — pin it to the
                        // commit it currently names, once, right here.
                        if let WrapSource::Git { url, revision } = &mut file.source {
                            if !is_full_sha(revision) {
                                let repo = Repo(Url::parse(url).wrap_err_with(|| {
                                    format!(
                                        "`{url}` (the `[wrap-git]` url for `{name}`) is not a URL"
                                    )
                                })?);
                                *revision = git_cache::resolve_rev(&repo, revision).wrap_err_with(
                                    || {
                                        format!(
                                            "Failed to resolve `{revision}` for the `{name}` wrap"
                                        )
                                    },
                                )?;
                            }
                        }
                        info!(name, version = resolved_version, "locked");
                        lock.wraps.retain(|w| &w.name != name);
                        lock.wraps.push(LockedWrap::new(
                            name,
                            version.as_deref(),
                            &resolved_version,
                            &file,
                        ));
                        dirty = true;
                        (resolved_version, file)
                    }
                };
                out.push(Resolved::Wrap {
                    version: resolved_version,
                    file,
                });
            }
        }
    }

    if dirty {
        save(path, &lock)?;
    }
    Ok(out)
}

/// Include the local wrap and its overlay in the materialization key.  Unlike
/// wrapdb, local files are deliberately editable, so reusing an old cache
/// entry after changing `packagefiles/` would be surprising.
fn local_fingerprint(wrap: &Path, overlay: Option<&Path>) -> String {
    let mut hash = Sha256::new();
    hash.update(fs::read(wrap).expect("local wrap was just read"));
    if let Some(overlay) = overlay {
        for file in files_under(overlay) {
            hash.update(
                file.strip_prefix(overlay)
                    .unwrap()
                    .to_string_lossy()
                    .as_bytes(),
            );
            hash.update(fs::read(file).expect("local overlay was just checked"));
        }
    }
    hex::encode(hash.finalize())
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("local overlay was just checked") {
        let path = entry.expect("local overlay entry").path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
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
            fs,
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

    fn git_project(repo: &str, reference: GitReference) -> Project {
        Project {
            source: Source::Git {
                repo: Repo(Url::parse(repo).unwrap()),
                reference,
            },
            options: Default::default(),
            host_machine: Machine::default(),
            build_machine: Machine::default(),
            depends: Vec::new(),
        }
    }

    fn wrap_project(name: &str, version: Option<&str>) -> Project {
        Project {
            source: Source::Wrap {
                name: name.to_owned(),
                version: version.map(str::to_owned),
            },
            options: Default::default(),
            host_machine: Machine::default(),
            build_machine: Machine::default(),
            depends: Vec::new(),
        }
    }

    #[test]
    fn a_local_wrap_is_preferred_and_keeps_its_overlay() {
        let root = std::env::temp_dir().join(format!("decay-local-wrap-lock-{}", process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("packagefiles/zlib")).unwrap();
        fs::write(root.join("packagefiles/zlib/meson.build"), "project('z')\n").unwrap();
        fs::write(
            root.join("zlib.wrap"),
            "[wrap-file]\nsource_url = https://example.test/zlib.tar.gz\nsource_filename = zlib.tar.gz\nsource_hash = deadbeef\npatch_directory = packagefiles/zlib\n",
        )
        .unwrap();

        let resolved = resolve(
            &tmp_lock_path("local-wrap"),
            &[wrap_project("zlib", None)],
            &tmp_git_cache(),
            Some(&root),
        )
        .unwrap();
        match &resolved[0] {
            Resolved::Wrap { version, file } => {
                assert!(version.starts_with("local-deadbeef-"));
                assert_eq!(
                    file.local_overlay.as_deref(),
                    Some(root.join("packagefiles/zlib").as_path())
                );
            }
            Resolved::Git { .. } => panic!("expected local wrap"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_full_sha_resolves_without_touching_the_lock_file() {
        let path = tmp_lock_path("full-sha");
        let sha = "a".repeat(40);
        let projects = [git_project(
            "https://example.test/x.git",
            GitReference::Rev(sha.clone()),
        )];

        let resolved = resolve(&path, &projects, &tmp_git_cache(), None).unwrap();

        assert!(matches!(&resolved[0], Resolved::Git { rev } if *rev == sha));
        assert!(!path.exists(), "a fully-pinned rev needs no lock entry");
    }

    #[test]
    fn a_locked_branch_is_reused_without_re_resolving() {
        let path = tmp_lock_path("reuse-git");
        let repo = "https://example.test/y.git";
        let resolved_sha = "b".repeat(40);
        save(
            &path,
            &LockFile {
                projects: vec![LockedGit {
                    repo: repo.to_owned(),
                    branch: Some("main".to_owned()),
                    tag: None,
                    rev: None,
                    resolved: resolved_sha.clone(),
                }],
                wraps: vec![],
            },
        )
        .unwrap();

        // No real `main` branch exists at this URL, so if this reached the
        // network path it would fail rather than quietly resolve to
        // something else.
        let resolved = resolve(
            &path,
            &[git_project(repo, GitReference::Branch("main".to_owned()))],
            &tmp_git_cache(),
            None,
        )
        .unwrap();

        assert!(matches!(&resolved[0], Resolved::Git { rev } if *rev == resolved_sha));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_branch_and_tag_with_the_same_name_do_not_share_a_lock_entry() {
        let path = tmp_lock_path("ref-kind");
        let repo = "https://example.test/y.git";
        let branch_sha = "b".repeat(40);
        save(
            &path,
            &LockFile {
                projects: vec![LockedGit {
                    repo: repo.to_owned(),
                    branch: Some("release".to_owned()),
                    tag: None,
                    rev: None,
                    resolved: branch_sha,
                }],
                wraps: vec![],
            },
        )
        .unwrap();

        let err = resolve(
            &path,
            &[git_project(repo, GitReference::Tag("release".to_owned()))],
            &tmp_git_cache(),
            None,
        )
        .err()
        .expect("a tag must not reuse a branch lock entry");
        assert!(format!("{err:#}").contains("tag `release`"));
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
            local_overlay: None,
        };
        save(
            &path,
            &LockFile {
                projects: vec![],
                wraps: vec![LockedWrap::new("zlib", None, "1.3.1-1", &file)],
            },
        )
        .unwrap();

        let resolved =
            resolve(&path, &[wrap_project("zlib", None)], &tmp_git_cache(), None).unwrap();

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
            local_overlay: None,
        };
        save(
            &path,
            &LockFile {
                projects: vec![],
                wraps: vec![LockedWrap::new("x", None, "1.0-1", &file)],
            },
        )
        .unwrap();

        // No real `x.git` exists at this URL, so if this reached the network
        // path (re-resolving `revision`) it would fail rather than quietly
        // reuse the pin.
        let resolved = resolve(&path, &[wrap_project("x", None)], &tmp_git_cache(), None).unwrap();

        match &resolved[0] {
            Resolved::Wrap { file, .. } => assert_eq!(
                file.source,
                WrapSource::Git {
                    url: "https://example.test/x.git".to_owned(),
                    revision: resolved_sha,
                }
            ),
            Resolved::Git { .. } => panic!("expected a wrap"),
        }
        let _ = fs::remove_file(&path);
    }
}
