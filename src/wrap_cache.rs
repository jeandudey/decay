//! Caching a wrapdb `[wrap-file]` source the same way [`crate::git_cache`]
//! caches a git checkout: downloaded and extracted once, verified against a
//! content hash, and reused after that. A `[wrap-git]` source is fetched by
//! `GitCache` itself; this only gets involved for one when it has a
//! `patch_directory` to overlay, since that can't be done in place on
//! `GitCache`'s shared checkout.

use {
    crate::{
        git_cache::GitCache,
        wrapdb::{self, WrapFile, WrapSource}, //
    },
    eyre::{
        Context,
        ContextCompat,
        bail, //
    },
    sha2::{
        Digest,
        Sha256, //
    },
    std::{
        fs,
        path::{
            Path,
            PathBuf, //
        },
        process::{self, Command},
    },
};

#[derive(Debug)]
pub struct WrapCache {
    root: PathBuf,
}

/// Where a materialized wrap landed, and what the generated build needs to
/// fetch the same tree itself.
pub struct Wrap {
    /// The project root to evaluate — the archive's single top-level
    /// directory, once stripped, or the extraction directory itself when the
    /// archive has none.
    pub dir: PathBuf,
    pub url: String,
    pub sha256: String,
    pub strip_prefix: Option<String>,
}

impl WrapCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Ensure a `[wrap-file]` wrap's source at `version` is downloaded,
    /// verified, extracted, and — when the wrap names a `patch_directory` —
    /// overlaid with wrapdb's own files for it.
    pub fn materialize(
        &self,
        git_cache: &GitCache,
        name: &str,
        version: &str,
        file: &WrapFile,
    ) -> eyre::Result<Wrap> {
        let WrapSource::Archive { url, filename, hash } = &file.source else {
            bail!("`{name}` at `{version}` is a `[wrap-git]` wrap, not a `[wrap-file]` one");
        };
        let archive = self.download(url, filename, hash)?;

        let dest = self.root.join("src").join(name).join(version);
        if !dest.join(".ok").is_file() {
            let overlay = file
                .patch_directory
                .as_deref()
                .map(|dir| wrapdb::patch_dir(git_cache, name, version, dir))
                .transpose()?;
            extract(&archive, filename, &dest, overlay.as_deref())?;
        }

        let strip_prefix = single_top_level_dir(&dest)?;
        let dir = match &strip_prefix {
            Some(prefix) => dest.join(prefix),
            None => dest.clone(),
        };
        Ok(Wrap {
            dir,
            url: url.clone(),
            sha256: hash.clone(),
            strip_prefix,
        })
    }

    /// Ensure a `[wrap-git]` wrap's `checkout` (already fetched by
    /// [`GitCache`]) is ready to evaluate. With no `overlay` (the wrap's
    /// resolved `patch_directory`, if it has one), that's `checkout`
    /// itself — nothing needs to mutate it, so there's no reason to copy it.
    /// With one, a private copy gets it applied instead, since `checkout` is
    /// `GitCache`'s shared worktree for that commit and every other project
    /// pinned to it trusts it unmodified.
    pub fn materialize_git(
        &self,
        name: &str,
        version: &str,
        checkout: &Path,
        overlay: Option<&Path>,
    ) -> eyre::Result<PathBuf> {
        let Some(overlay) = overlay else {
            return Ok(checkout.to_path_buf());
        };

        let dest = self.root.join("git").join(name).join(version);
        if !dest.join(".ok").is_file() {
            copy_and_overlay(checkout, overlay, &dest)?;
        }
        Ok(dest)
    }

    fn download(&self, url: &str, filename: &str, hash: &str) -> eyre::Result<PathBuf> {
        let dir = self.root.join("dl").join(&hash[..16.min(hash.len())]);
        let dest = dir.join(filename);
        if dest.is_file() && verify(&dest, hash).is_ok() {
            return Ok(dest);
        }

        fs::create_dir_all(&dir).wrap_err("Failed to create the download cache directory")?;
        let tmp = dir.join(format!(".tmp-{}", process::id()));
        wrapdb::download(url, &tmp).wrap_err_with(|| format!("Failed to download {url}"))?;
        verify(&tmp, hash).wrap_err_with(|| {
            format!("{url} does not match the hash `decay.lock` pins for it")
        })?;
        fs::rename(&tmp, &dest).wrap_err("Failed to publish the downloaded archive")?;
        Ok(dest)
    }
}

/// Extract `archive` into a fresh directory, overlay `overlay` onto it (when
/// given — a `patch_directory`'s files, copied the way meson's own
/// `copy_tree` does), and publish the result at `dest`, the same
/// rename-into-place pattern [`crate::git_cache`] uses so a partial
/// extraction never gets mistaken for a complete one.
fn extract(archive: &Path, filename: &str, dest: &Path, overlay: Option<&Path>) -> eyre::Result<()> {
    let parent = dest.parent().wrap_err("wrap destination has no parent")?;
    fs::create_dir_all(parent).wrap_err("Failed to create directory for wrap entry")?;

    let tmp = parent.join(format!(".tmp-{}", process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).wrap_err("Failed to create extraction directory")?;

    let status = if filename.ends_with(".zip") {
        Command::new("unzip").args(["-q"]).arg(archive).args(["-d"]).arg(&tmp).status()
    } else {
        // GNU tar auto-detects gzip/xz/bzip2/zstd from the archive itself.
        Command::new("tar").arg("xf").arg(archive).args(["-C"]).arg(&tmp).status()
    }
    .wrap_err("Failed to run the archive extractor")?;
    if !status.success() {
        let _ = fs::remove_dir_all(&tmp);
        bail!("failed to extract {}", archive.display());
    }

    if let Some(src) = overlay {
        let overlay_result = (|| {
            let target = match single_top_level_dir(&tmp)? {
                Some(prefix) => tmp.join(prefix),
                None => tmp.clone(),
            };
            copy_tree(src, &target)
        })();
        if let Err(e) = overlay_result {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
    }

    if dest.exists() {
        fs::remove_dir_all(dest).wrap_err("Failed to remove a stale wrap directory")?;
    }
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e).wrap_err("Failed to publish extracted archive");
    }
    fs::write(dest.join(".ok"), []).wrap_err("Failed to mark wrap extraction complete")
}

/// Copy `checkout` (a `[wrap-git]` fetch) into a fresh directory, overlay
/// `patch` onto it, and publish the result at `dest` — the same
/// tmp-then-rename pattern `extract` uses.
fn copy_and_overlay(checkout: &Path, patch: &Path, dest: &Path) -> eyre::Result<()> {
    let parent = dest.parent().wrap_err("wrap destination has no parent")?;
    fs::create_dir_all(parent).wrap_err("Failed to create directory for wrap entry")?;

    let tmp = parent.join(format!(".tmp-{}", process::id()));
    let _ = fs::remove_dir_all(&tmp);

    let result = copy_tree(checkout, &tmp).and_then(|()| copy_tree(patch, &tmp));
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }

    if dest.exists() {
        fs::remove_dir_all(dest).wrap_err("Failed to remove a stale wrap directory")?;
    }
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e).wrap_err("Failed to publish overlaid checkout");
    }
    fs::write(dest.join(".ok"), []).wrap_err("Failed to mark wrap checkout complete")
}

/// Overlay every file in `src` onto `dst`, overwriting what's already there —
/// the same recursive copy meson's own `PackageDefinition.copy_tree` applies
/// a `patch_directory` with. Skips a top-level `.git`: irrelevant either way
/// (a stray pointer file from a `GitCache` worktree, or absent from a
/// `patch_directory`), and never something a build should read.
fn copy_tree(src: &Path, dst: &Path) -> eyre::Result<()> {
    fs::create_dir_all(dst).wrap_err_with(|| format!("Failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).wrap_err_with(|| format!("Failed to read {}", src.display()))? {
        let entry = entry.wrap_err("Failed to read a directory entry")?;
        if entry.file_name() == ".git" {
            continue;
        }
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type().wrap_err("Failed to stat a directory entry")?;
        if file_type.is_dir() {
            copy_tree(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), &dst_path).wrap_err_with(|| {
                format!(
                    "Failed to overlay {} onto {}",
                    entry.path().display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn verify(path: &Path, expected_sha256: &str) -> eyre::Result<()> {
    let bytes = fs::read(path).wrap_err_with(|| format!("Failed to read {}", path.display()))?;
    let got = hex::encode(Sha256::digest(&bytes));
    if !got.eq_ignore_ascii_case(expected_sha256) {
        bail!(
            "hash mismatch for {}: expected {expected_sha256}, got {got}",
            path.display()
        );
    }
    Ok(())
}

/// The single top-level directory an archive extracted into, if it has one —
/// the layout every wrapdb tarball uses (`zlib-1.3.1/...`), but not one this
/// cache should assume rather than check.
fn single_top_level_dir(dest: &Path) -> eyre::Result<Option<String>> {
    let mut entries: Vec<_> = fs::read_dir(dest)
        .wrap_err_with(|| format!("Failed to read {}", dest.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != ".ok")
        .collect();
    if entries.len() == 1 && entries[0].path().is_dir() {
        return Ok(Some(entries.remove(0).file_name().to_string_lossy().into_owned()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::wrapdb, //
    };

    /// Not run by default (hits the real network and wrapdb) — a manual
    /// check that a `patch_directory` wrap actually overlays correctly.
    /// `cargo test -- --ignored materializes_pcre2_with_its_overlay`
    #[test]
    #[ignore = "network"]
    fn materializes_pcre2_with_its_overlay() {
        let root = std::env::temp_dir().join("decay-wrapdb-smoke-test");
        let _ = fs::remove_dir_all(&root);
        let git_cache = GitCache::new(root.join("git"));
        let wrap_cache = WrapCache::new(root.join("wrap"));

        let file = wrapdb::fetch(&git_cache, "pcre2", "10.48-1").unwrap();
        assert_eq!(file.patch_directory.as_deref(), Some("pcre2"));

        let wrap = wrap_cache.materialize(&git_cache, "pcre2", "10.48-1", &file).unwrap();

        let meson_build =
            fs::read_to_string(wrap.dir.join("meson.build")).expect("overlaid meson.build");
        // pcre2's upstream release tarball ships an autotools build, not a
        // meson one — this file only exists because the overlay put it
        // there.
        assert!(meson_build.contains("'pcre2'"), "not wrapdb's meson.build:\n{meson_build}");
    }

    /// No live wrapdb release is currently a `[wrap-git]` wrap to test
    /// discovery against (wrapdb migrated every one to `[wrap-file]` +
    /// `patch_directory`; even the one historical example this was checked
    /// against, `ff-nvcodec-headers_11.1.5.1-0`, had its tag force-moved to
    /// a `[wrap-file]` wrap later) — so this exercises `materialize_git`
    /// directly against a throwaway local git repo instead, entirely
    /// offline, which is what actually changed here (`wrapdb::parse_wrap`
    /// parsing `[wrap-git]` is covered without any repo at all, in
    /// `wrapdb::tests`).
    #[test]
    fn materialize_git_overlays_without_touching_the_shared_checkout() {
        let root = std::env::temp_dir().join(format!("decay-wrap-git-test-{}", process::id()));
        let _ = fs::remove_dir_all(&root);

        let upstream = root.join("upstream.git");
        run(Command::new("git").args(["init", "--quiet", "-b", "main"]).arg(&upstream));
        run(Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--quiet", "--allow-empty", "-m", "x"])
            .current_dir(&upstream));

        let git_cache = GitCache::new(root.join("cache"));
        let wrap_cache = WrapCache::new(root.join("wrap"));
        let repo = crate::config::Repo(format!("file://{}", upstream.display()).parse().unwrap());
        let resolved = crate::git_cache::resolve_rev(&repo, "main").unwrap();
        let checkout = git_cache.checkout(&repo, &resolved).unwrap();

        let overlay = root.join("overlay");
        fs::create_dir_all(&overlay).unwrap();
        fs::write(overlay.join("meson.build"), "project('x')\n").unwrap();

        let dir = wrap_cache.materialize_git("x", "1.0-1", &checkout, Some(&overlay)).unwrap();

        // Must not be the shared `GitCache` checkout itself — mutating that
        // in place would corrupt it for every other consumer of that commit.
        assert_ne!(dir, checkout);
        assert!(!checkout.join("meson.build").exists(), "the shared checkout was mutated");
        let meson_build = fs::read_to_string(dir.join("meson.build")).unwrap();
        assert_eq!(meson_build, "project('x')\n");

        // No overlay at all reuses the shared checkout directly.
        let dir = wrap_cache.materialize_git("x", "1.0-1", &checkout, None).unwrap();
        assert_eq!(dir, checkout);

        let _ = fs::remove_dir_all(&root);
    }

    fn run(cmd: &mut Command) {
        let status = cmd.status().unwrap();
        assert!(status.success(), "{cmd:?} failed");
    }
}
