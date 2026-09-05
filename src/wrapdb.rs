//! Discovering and fetching a [meson wrapdb](https://github.com/mesonbuild/wrapdb)
//! `.wrap` file — both kinds, `[wrap-file]` (a tarball) and `[wrap-git]` (a
//! git repository and revision).
//!
//! wrapdb publishes itself as a git repository, not a stable API: its old
//! `v2` query endpoints answer with data that doesn't match what a current
//! `.wrap` file says (they know nothing of `patch_directory`, for one). The
//! repository itself is the source of truth instead — `releases.json` at
//! its root lists every project's known versions, newest first, and each
//! `name`/`version` pair is tagged `{name}_{version}`, at which
//! `subprojects/{name}.wrap` and, when the wrap carries one,
//! `subprojects/packagefiles/{patch_directory}` are exactly what that
//! release resolved to. `decay` checks it out through
//! [`crate::git_cache::GitCache`], the same way it checks out any other
//! project's history, rather than a second fetching path of its own.
//!
//! A `[wrap-git]` entry names a git repository and a revision — exactly what
//! an ordinary `[[project]]` already is — so [`crate::lock`] and
//! [`crate::wrap_cache`] resolve and fetch it exactly the way they would a
//! `[[project]]` `repo`/`rev`, rather than a second, narrower path to the
//! same thing.

use {
    crate::{
        config::Repo,
        git_cache::{self, GitCache}, //
    },
    eyre::{
        Context,
        bail,
        eyre, //
    },
    std::{
        collections::BTreeMap,
        fs,
        path::{
            Path,
            PathBuf, //
        },
        process::Command,
        sync::LazyLock, //
    },
    url::Url,
};

/// Where wrapdb itself lives, checked out like any other `[[project]]`.
pub static REPO: LazyLock<Repo> =
    LazyLock::new(|| Repo(Url::parse("https://github.com/mesonbuild/wrapdb.git").expect("static URL")));

/// What one `.wrap` file promises, once parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapFile {
    pub source: WrapSource,
    /// A directory in wrapdb's own tree
    /// (`subprojects/packagefiles/<patch_directory>`) to overlay onto the
    /// fetched source, file by file, the way meson's own `copy_tree` does —
    /// applies after either kind of `source` is fetched.
    /// [`crate::wrap_cache::WrapCache`] is what applies it.
    pub patch_directory: Option<String>,
}

/// Where a wrap's source actually comes from — one `.wrap` file has exactly
/// one of these, named by which section it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WrapSource {
    /// `[wrap-file]`: a tarball to download and extract.
    Archive {
        url: String,
        filename: String,
        hash: String,
    },
    /// `[wrap-git]`: a git repository and revision. `revision` is whatever
    /// the wrap named — a branch, tag, or commit — not yet resolved to a
    /// full hash; [`crate::lock`] is what does that, the same as it does for
    /// an ordinary `[[project]]`'s own `rev`.
    Git { url: String, revision: String },
}

/// The newest version wrapdb's `releases.json` lists for `name`.
pub fn latest_version(git_cache: &GitCache, name: &str) -> eyre::Result<String> {
    let checkout = checkout_rev(git_cache, "master")?;
    let text = fs::read_to_string(checkout.join("releases.json"))
        .wrap_err("wrapdb checkout has no releases.json")?;
    let releases: serde_json::Value =
        serde_json::from_str(&text).wrap_err("wrapdb's releases.json is not valid JSON")?;
    releases[name]["versions"][0]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| eyre!("wrapdb's releases.json does not list a project named `{name}`"))
}

/// Fetch and parse the `.wrap` file wrapdb published for `name` at `version`
/// (wrapdb's own `version-revision` spelling, e.g. `1.3.1-1`) — the state of
/// `subprojects/{name}.wrap` at the git tag `{name}_{version}`.
pub fn fetch(git_cache: &GitCache, name: &str, version: &str) -> eyre::Result<WrapFile> {
    let checkout = checkout_release(git_cache, name, version)?;
    let path = checkout.join("subprojects").join(format!("{name}.wrap"));
    let text = fs::read_to_string(&path).wrap_err_with(|| {
        format!(
            "wrapdb has no `{}` at `{name}_{version}` — this version may predate decay's \
             supported layout",
            path.display()
        )
    })?;
    parse_wrap(&text)
}

/// The overlay directory a `patch_directory` names, once wrapdb is checked
/// out at `name`/`version`.
pub fn patch_dir(git_cache: &GitCache, name: &str, version: &str, dir: &str) -> eyre::Result<PathBuf> {
    let checkout = checkout_release(git_cache, name, version)?;
    let path = checkout.join("subprojects").join("packagefiles").join(dir);
    if !path.is_dir() {
        bail!(
            "`{name}_{version}` names patch_directory `{dir}`, but `subprojects/packagefiles/{dir}` \
             does not exist in wrapdb"
        );
    }
    Ok(path)
}

/// Download a wrap's `source_url` straight to `dest`, verbatim — the actual
/// project tarball, which lives wherever upstream hosts it, not in wrapdb's
/// own repository.
pub fn download(url: &str, dest: &Path) -> eyre::Result<()> {
    let out = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .output()
        .wrap_err("Failed to run `curl`")?;
    if !out.status.success() {
        bail!(
            "curl failed downloading {url}:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn checkout_release(git_cache: &GitCache, name: &str, version: &str) -> eyre::Result<PathBuf> {
    checkout_rev(git_cache, &format!("{name}_{version}"))
}

/// Check `rev` (a branch or tag name) out of wrapdb.
///
/// [`GitCache::checkout`] only takes a full commit hash reliably — a bare
/// branch or tag name doesn't resolve against the local mirror it fetches,
/// the same reason [`crate::lock`] resolves a `[[project]]`'s own `rev`
/// through [`git_cache::resolve_rev`] before ever calling it — so this does
/// that first.
fn checkout_rev(git_cache: &GitCache, rev: &str) -> eyre::Result<PathBuf> {
    let sha = git_cache::resolve_rev(&REPO, rev)
        .wrap_err_with(|| format!("Failed to resolve `{rev}` in wrapdb"))?;
    git_cache
        .checkout(&REPO, &sha)
        .wrap_err_with(|| format!("Failed to check out wrapdb at `{rev}`"))
}

fn parse_wrap(text: &str) -> eyre::Result<WrapFile> {
    let sections = parse_ini(text);

    if let Some(git) = sections.get("wrap-git") {
        let get = |key: &str| {
            git.get(key)
                .cloned()
                .ok_or_else(|| eyre!("`[wrap-git]` has no `{key}`"))
        };
        return Ok(WrapFile {
            source: WrapSource::Git {
                url: get("url")?,
                revision: get("revision")?,
            },
            patch_directory: git.get("patch_directory").cloned(),
        });
    }

    let file = sections
        .get("wrap-file")
        .ok_or_else(|| eyre!("this `.wrap` has neither a `[wrap-file]` nor a `[wrap-git]` section"))?;
    let get = |key: &str| {
        file.get(key)
            .cloned()
            .ok_or_else(|| eyre!("`[wrap-file]` has no `{key}`"))
    };
    if file.contains_key("patch_filename") {
        bail!(
            "this wrap overlays a legacy `patch_filename` archive, which decay does not support \
             (see the wrap known gap in AGENTS.md); every current wrapdb release uses \
             `patch_directory` instead"
        );
    }
    Ok(WrapFile {
        source: WrapSource::Archive {
            url: get("source_url")?,
            filename: get("source_filename")?,
            hash: get("source_hash")?,
        },
        patch_directory: file.get("patch_directory").cloned(),
    })
}

/// The handful of `key = value` lines under `[section]` headers a `.wrap`
/// file is written in — not full INI (no quoting, no escapes, no line
/// continuations), which is all meson's own writer ever produces.
fn parse_ini(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.to_owned();
            sections.entry(current.clone()).or_default();
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            sections
                .entry(current.clone())
                .or_default()
                .insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_wrap_file_without_a_patch() {
        let text = "[wrap-file]\n\
             directory = zlib-1.3.1\n\
             source_url = https://example.com/zlib-1.3.1.tar.gz\n\
             source_filename = zlib-1.3.1.tar.gz\n\
             source_hash = deadbeef\n\
             \n\
             [provide]\n\
             dependency_names = zlib\n";
        let wrap = parse_wrap(text).unwrap();
        assert_eq!(wrap.source, WrapSource::Archive {
            url: "https://example.com/zlib-1.3.1.tar.gz".to_owned(),
            filename: "zlib-1.3.1.tar.gz".to_owned(),
            hash: "deadbeef".to_owned(),
        });
        assert_eq!(wrap.patch_directory, None);
    }

    #[test]
    fn a_patch_directory_is_captured() {
        let text = "[wrap-file]\n\
             source_url = https://example.com/x.tar.gz\n\
             source_filename = x.tar.gz\n\
             source_hash = deadbeef\n\
             patch_directory = x\n";
        assert_eq!(parse_wrap(text).unwrap().patch_directory.as_deref(), Some("x"));
    }

    #[test]
    fn a_legacy_patch_filename_is_refused() {
        let text = "[wrap-file]\n\
             source_url = https://example.com/x.tar.gz\n\
             source_filename = x.tar.gz\n\
             source_hash = deadbeef\n\
             patch_filename = x_1-1_patch.zip\n\
             patch_url = https://example.com/x_1-1_patch.zip\n\
             patch_hash = cafef00d\n";
        let err = parse_wrap(text).unwrap_err().to_string();
        assert!(err.contains("patch_filename"), "{err}");
    }

    #[test]
    fn a_wrap_git_entry_is_parsed() {
        let text = "[wrap-git]\n\
             url = https://example.com/x.git\n\
             revision = main\n";
        let wrap = parse_wrap(text).unwrap();
        assert_eq!(wrap.source, WrapSource::Git {
            url: "https://example.com/x.git".to_owned(),
            revision: "main".to_owned(),
        });
        assert_eq!(wrap.patch_directory, None);
    }

    #[test]
    fn a_wrap_git_entry_with_a_patch_directory_is_parsed() {
        // A real wrapdb example (`ff-nvcodec-headers`): `[wrap-git]` can
        // carry a `patch_directory` too, applied after the checkout the same
        // way it would be after an archive extraction.
        let text = "[wrap-git]\n\
             url = https://example.com/x.git\n\
             revision = v1.0\n\
             patch_directory = x\n";
        assert_eq!(parse_wrap(text).unwrap().patch_directory.as_deref(), Some("x"));
    }

    #[test]
    fn a_wrap_with_neither_section_is_an_error() {
        let text = "[provide]\ndependency_names = x\n";
        let err = parse_wrap(text).unwrap_err().to_string();
        assert!(err.contains("wrap-file") && err.contains("wrap-git"), "{err}");
    }
}
