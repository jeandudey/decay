use {
    crate::config::{
        Repo,
        is_full_sha, //
    },
    eyre::{
        Context,
        ContextCompat,
        bail,
        eyre, //
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
pub struct GitCache {
    root: PathBuf,
}

impl GitCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `rev` must already be a full commit hash — [`crate::lock`] is what
    /// turns a `decay.toml` branch or tag into one of these and pins it, so
    /// the cache never has to decide whether a moving name still points where
    /// it used to.
    pub fn checkout(&self, repo: &Repo, rev: &str) -> eyre::Result<PathBuf> {
        let ident = repo.ident()?;
        let db = self.db_dir(&ident);

        if is_full_sha(rev) {
            let dest = self.checkouts_dir(&ident).join(&rev[16..]);
            if dest.join(".ok").exists() {
                return Ok(dest);
            }
        }

        self.ensure_db(&db, repo, rev)?;

        let oid = run_git(
            git()
                .arg("--git-dir")
                .arg(&db)
                .arg("rev-parse")
                .arg(format!("{rev}^{{commit}}")),
        )
        .wrap_err_with(|| format!("`{rev}` is not a commit in {}", repo.0))?;

        let dest = self.checkouts_dir(&ident).join(&oid[..16]);
        if dest.join(".ok").is_file() {
            return Ok(dest);
        }

        self.materialize(&db, &oid, &dest)?;
        Ok(dest)
    }

    fn ensure_db(&self, db: &Path, repo: &Repo, rev: &str) -> eyre::Result<()> {
        if !db.join("HEAD").is_file() {
            fs::create_dir_all(db).wrap_err("Failed to create database directory")?;
            run_git(git().args(["init", "--quiet", "--bare"]).arg(db))
                .wrap_err("Failed to initialize git repository")?;
        } else if has_commit(db, rev) {
            return Ok(());
        }

        // A project pinned to a full commit hash names exactly the one object
        // worth having, so fetch only that, shallowly, instead of every
        // branch's entire history. GitHub and GitLab both serve an arbitrary
        // reachable commit this way; a host that refuses falls through to the
        // full fetch below.
        if is_full_sha(rev) {
            let ref_name = format!("refs/decay/{rev}");
            let shallow = git()
                .arg("--git-dir")
                .arg(db)
                .args(["fetch", "--depth", "1", "--quiet"])
                .arg(repo.0.to_string())
                .arg(format!("{rev}:{ref_name}"))
                .output();
            if shallow.is_ok_and(|out| out.status.success()) && has_commit(db, rev) {
                return Ok(());
            }
        }

        run_git(
            git()
                .arg("--git-dir")
                .arg(db)
                .args(["fetch", "--force", "--tags", "--quiet"])
                .arg(repo.0.to_string())
                .arg("+refs/heads/*:refs/remotes/origin/*"),
        )
        .wrap_err_with(|| format!("Failed to fetch {}", repo.0))?;

        Ok(())
    }

    fn materialize(&self, db: &Path, oid: &str, dest: &Path) -> eyre::Result<()> {
        let parent = dest
            .parent()
            .wrap_err("Failed to retrieve parent for database")?;
        fs::create_dir_all(parent).wrap_err("Failed to create directory for database entry")?;

        let tmp = parent.join(format!(".tmp-{}-{oid}", process::id()));
        let _ = fs::remove_dir_all(&tmp);

        // A worktree checks out `oid` straight from the shared object store: no
        // second copy of the repository, unlike a local clone of a shallow
        // `db` (git refuses to hardlink one of those, and falls back to
        // copying every object again).
        let _ = git()
            .arg("--git-dir")
            .arg(db)
            .args(["worktree", "prune", "--quiet"])
            .output();
        run_git(
            git()
                .arg("--git-dir")
                .arg(db)
                .args(["worktree", "add", "--quiet", "--detach"])
                .arg(&tmp)
                .arg(oid),
        )?;

        fs::write(tmp.join(".ok"), [])?;

        if dest.exists() {
            fs::remove_dir_all(dest)?;
        }

        if let Err(e) = fs::rename(&tmp, dest) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e).wrap_err("Failed to publish checkout");
        }

        Ok(())
    }

    fn db_dir(&self, ident: impl AsRef<Path>) -> PathBuf {
        self.root.join("git/db").join(ident)
    }

    fn checkouts_dir(&self, ident: impl AsRef<Path>) -> PathBuf {
        self.root.join("git/checkouts").join(ident)
    }
}

/// What `rev` (a branch, tag, or already-full commit hash) currently names in
/// `repo`, without fetching or caching anything — [`crate::lock`] is what
/// pins the answer afterward, the way `Cargo.lock` pins what a version
/// requirement resolved to.
pub fn resolve_rev(repo: &Repo, rev: &str) -> eyre::Result<String> {
    if is_full_sha(rev) {
        return Ok(rev.to_owned());
    }

    let out = run_git(
        git()
            .args(["ls-remote", "--exit-code"])
            .arg(repo.0.to_string())
            .arg(rev),
    )
    .wrap_err_with(|| format!("`{rev}` is not a ref in {}", repo.0))?;

    // An annotated tag lists both the tag object and, on a second line, the
    // commit it points at (suffixed `^{}`); a lightweight tag or branch has
    // only the one line already naming a commit. Either way, prefer the
    // peeled line when there is one.
    out.lines()
        .find(|line| line.ends_with("^{}"))
        .or_else(|| out.lines().next())
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .ok_or_else(|| eyre!("`git ls-remote {} {rev}` returned nothing", repo.0))
}

fn has_commit(db: &Path, rev: &str) -> bool {
    run_git(
        git()
            .arg("--git-dir")
            .arg(db)
            .args(["cat-file", "-e"])
            .arg(format!("{rev}^{{commit}}")),
    )
    .is_ok()
}

fn run_git(command: &mut Command) -> eyre::Result<String> {
    let out = command.output().wrap_err("Failed to run `git`")?;

    if !out.status.success() {
        bail!(
            "git failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn git() -> Command {
    Command::new("git")
}
