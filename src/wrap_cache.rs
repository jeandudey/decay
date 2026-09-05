//! Caching a wrapdb `[wrap-file]` source the same way [`crate::git_cache`]
//! caches a git checkout: downloaded and extracted once, verified against a
//! content hash, and reused after that.

use {
    crate::{
        git_cache::GitCache,
        wrapdb::{self, WrapFile}, //
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

    /// Ensure `name`'s wrap-file source at `version` is downloaded, verified,
    /// extracted, and — when the wrap names a `patch_directory` — overlaid
    /// with wrapdb's own files for it.
    pub fn materialize(
        &self,
        git_cache: &GitCache,
        name: &str,
        version: &str,
        file: &WrapFile,
    ) -> eyre::Result<Wrap> {
        let archive = self.download(file)?;

        let dest = self.root.join("src").join(name).join(version);
        if !dest.join(".ok").is_file() {
            let overlay = file
                .patch_directory
                .as_deref()
                .map(|dir| wrapdb::patch_dir(git_cache, name, version, dir))
                .transpose()?;
            extract(&archive, &file.source_filename, &dest, overlay.as_deref())?;
        }

        let strip_prefix = single_top_level_dir(&dest)?;
        let dir = match &strip_prefix {
            Some(prefix) => dest.join(prefix),
            None => dest.clone(),
        };
        Ok(Wrap {
            dir,
            url: file.source_url.clone(),
            sha256: file.source_hash.clone(),
            strip_prefix,
        })
    }

    fn download(&self, file: &WrapFile) -> eyre::Result<PathBuf> {
        let dir = self.root.join("dl").join(&file.source_hash[..16.min(file.source_hash.len())]);
        let dest = dir.join(&file.source_filename);
        if dest.is_file() && verify(&dest, &file.source_hash).is_ok() {
            return Ok(dest);
        }

        fs::create_dir_all(&dir).wrap_err("Failed to create the download cache directory")?;
        let tmp = dir.join(format!(".tmp-{}", process::id()));
        wrapdb::download(&file.source_url, &tmp)
            .wrap_err_with(|| format!("Failed to download {}", file.source_url))?;
        verify(&tmp, &file.source_hash).wrap_err_with(|| {
            format!(
                "{} does not match the hash `decay.lock` pins for it",
                file.source_url
            )
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

/// Overlay every file in `src` onto `dst`, overwriting what's already there —
/// the same recursive copy meson's own `PackageDefinition.copy_tree` applies
/// a `patch_directory` with.
fn copy_tree(src: &Path, dst: &Path) -> eyre::Result<()> {
    fs::create_dir_all(dst).wrap_err_with(|| format!("Failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).wrap_err_with(|| format!("Failed to read {}", src.display()))? {
        let entry = entry.wrap_err("Failed to read a directory entry")?;
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
mod network_tests {
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
}
