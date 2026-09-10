//! Deterministic answers for meson `run_command()` — consumed by
//! [`crate::oracle::ConfigOracle`]. Nothing here reads the machine the
//! importer runs on: a `git describe` synthesized from the ref decay already
//! pinned, or one of a few read-only commands whose output is a pure function
//! of the pinned checkout.

use {
    crate::config::GitReference,
    decay_meson_eval::oracle::RunAnswer,
    std::{
        path::{
            Component,
            Path, //
        },
        process::Command,
    },
};

/// `git describe [...]` from the ref decay resolved, without touching the
/// checkout's `.git` (a shallow mirror may not even carry tags).
pub fn git_describe(reference: &GitReference, argv: &[String]) -> RunAnswer {
    let always = argv.iter().any(|a| a == "--always");
    match reference {
        // An exact-tag checkout: `git describe` prints the bare tag.
        GitReference::Tag(tag) => ok(tag.clone()),
        // No tag reachable. Real `git describe` exits 128 unless `--always`,
        // which then prints the abbreviated commit.
        GitReference::Rev(rev) if always => ok(rev.chars().take(12).collect()),
        GitReference::Branch(_) | GitReference::Rev(_) => RunAnswer {
            code: 128,
            stdout: String::new(),
            stderr: String::new(),
        },
    }
}

fn ok(stdout: String) -> RunAnswer {
    RunAnswer {
        code: 0,
        stdout,
        stderr: String::new(),
    }
}

/// Commands decay will actually run: output is a pure function of the pinned
/// tree, no machine or environment state.
// ponytail: fixed set; extend as real projects turn up more.
const ALLOWED: &[&str] = &["cat", "head", "tail", "echo", "true", "false"];

/// Run `argv` in `root` iff it is allowlisted and no path argument escapes
/// `root`. `None` otherwise — the caller then refuses the call.
pub fn run_readonly(root: &Path, argv: &[String]) -> Option<RunAnswer> {
    let (cmd, rest) = argv.split_first()?;
    let cmd = Path::new(cmd).file_name()?.to_str()?;
    if !ALLOWED.contains(&cmd) {
        return None;
    }
    for arg in rest {
        if arg.starts_with('-') {
            continue;
        }
        let p = Path::new(arg);
        if p.is_absolute() || p.components().any(|c| c == Component::ParentDir) {
            return None;
        }
    }
    let out = Command::new(cmd)
        .args(rest)
        .current_dir(root)
        .output()
        .ok()?;
    Some(RunAnswer {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_synth() {
        let tag = GitReference::Tag("v2.15.4".into());
        assert_eq!(
            git_describe(&tag, &["git".into(), "describe".into()]).stdout,
            "v2.15.4"
        );

        let rev = GitReference::Rev("a".repeat(40));
        assert_eq!(
            git_describe(&rev, &["git".into(), "describe".into()]).code,
            128
        );
        assert_eq!(
            git_describe(&rev, &["git".into(), "describe".into(), "--always".into()]).stdout,
            "aaaaaaaaaaaa"
        );
    }

    #[test]
    fn readonly_allowlist_and_containment() {
        let dir = std::env::temp_dir().join(format!("decay-runcmd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("VERSION"), "1.2.3\n").unwrap();

        let hit = run_readonly(&dir, &["cat".into(), "VERSION".into()]).unwrap();
        assert_eq!(hit.stdout, "1.2.3\n");
        assert_eq!(hit.code, 0);

        assert!(run_readonly(&dir, &["date".into()]).is_none());
        assert!(run_readonly(&dir, &["cat".into(), "../secret".into()]).is_none());
        assert!(run_readonly(&dir, &["cat".into(), "/etc/hostname".into()]).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
