//! A client for the [meson wrapdb](https://wrapdb.mesonbuild.com), and the
//! small INI dialect a `.wrap` file is written in.
//!
//! Only `[wrap-file]` is understood. Wrapdb's other kind, `[wrap-git]`, names
//! a git repository and a revision — exactly what an ordinary `[[project]]`
//! already is — so it belongs in `decay.toml` as one of those instead of a
//! second, narrower path to the same thing.

use {
    eyre::{
        Context,
        bail,
        eyre, //
    },
    std::{
        collections::BTreeMap,
        path::Path,
        process::Command, //
    },
};

const BASE: &str = "https://wrapdb.mesonbuild.com/v2";

/// What one `[wrap-file]` promises, once parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapFile {
    pub source_url: String,
    pub source_filename: String,
    pub source_hash: String,
    /// Whether this wrap also carries a patch archive to overlay on the
    /// source after extraction — refused rather than approximated; see the
    /// module docs on `[wrap-git]` for why the same reasoning applies here.
    pub has_patch: bool,
}

/// The latest release wrapdb offers for `name`, in wrapdb's own
/// `version-revision` spelling (e.g. `1.3.1-1`).
pub fn latest_version(name: &str) -> eyre::Result<String> {
    let json = curl_get(&format!("{BASE}/query/get_latest/{name}"))
        .wrap_err_with(|| format!("Failed to look up the latest wrapdb version of `{name}`"))?;
    let value: serde_json::Value =
        serde_json::from_str(&json).wrap_err("wrapdb did not return valid JSON")?;
    let branch = value["branch"]
        .as_str()
        .ok_or_else(|| eyre!("wrapdb's answer for `{name}` has no `branch`"))?;
    let revision = value["revision"]
        .as_i64()
        .ok_or_else(|| eyre!("wrapdb's answer for `{name}` has no `revision`"))?;
    Ok(format!("{branch}-{revision}"))
}

/// Fetch and parse the `.wrap` file for `name` at `version` (wrapdb's
/// `version-revision` spelling).
pub fn fetch(name: &str, version: &str) -> eyre::Result<WrapFile> {
    let text = curl_get(&format!("{BASE}/{name}/{version}/get_wrap"))
        .wrap_err_with(|| format!("Failed to fetch the `{name}` wrap at `{version}`"))?;
    parse_wrap(&text)
}

/// Download `url` straight to `dest`, verbatim.
pub fn download(url: &str, dest: &Path) -> eyre::Result<()> {
    let out = curl()
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

fn parse_wrap(text: &str) -> eyre::Result<WrapFile> {
    let sections = parse_ini(text);
    let file = sections.get("wrap-file").ok_or_else(|| {
        eyre!(
            "this is a `[wrap-git]` wrap, not a `[wrap-file]` one; import it as an ordinary \
             `[[project]]` (`repo`/`rev`) instead"
        )
    })?;
    let get = |key: &str| {
        file.get(key)
            .cloned()
            .ok_or_else(|| eyre!("`[wrap-file]` has no `{key}`"))
    };
    Ok(WrapFile {
        source_url: get("source_url")?,
        source_filename: get("source_filename")?,
        source_hash: get("source_hash")?,
        has_patch: file.contains_key("patch_filename"),
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

fn curl_get(url: &str) -> eyre::Result<String> {
    let out = curl().arg("-fsSL").arg(url).output().wrap_err("Failed to run `curl`")?;
    if !out.status.success() {
        bail!(
            "curl failed fetching {url}:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn curl() -> Command {
    Command::new("curl")
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
        assert_eq!(wrap.source_url, "https://example.com/zlib-1.3.1.tar.gz");
        assert_eq!(wrap.source_filename, "zlib-1.3.1.tar.gz");
        assert_eq!(wrap.source_hash, "deadbeef");
        assert!(!wrap.has_patch);
    }

    #[test]
    fn a_patch_is_detected() {
        let text = "[wrap-file]\n\
             source_url = https://example.com/x.tar.gz\n\
             source_filename = x.tar.gz\n\
             source_hash = deadbeef\n\
             patch_filename = x_1-1_patch.zip\n\
             patch_url = https://example.com/x_1-1_patch.zip\n\
             patch_hash = cafef00d\n";
        assert!(parse_wrap(text).unwrap().has_patch);
    }

    #[test]
    fn a_wrap_git_entry_is_rejected() {
        let text = "[wrap-git]\n\
             url = https://example.com/x.git\n\
             revision = main\n";
        let err = parse_wrap(text).unwrap_err().to_string();
        assert!(err.contains("wrap-git"), "{err}");
    }
}
