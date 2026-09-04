use {
    crate::config::Project,
    eyre::{
        Context,
        ContextCompat,
        bail, //
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

    pub fn checkout(&self, project: &Project) -> eyre::Result<PathBuf> {
        let ident = project.repo.ident()?;
        let db = self.db_dir(&ident);

        if project.is_full_sha() {
            let dest = self.checkouts_dir(&ident).join(&project.rev[16..]);
            if dest.join(".ok").exists() {
                return Ok(dest);
            }
        }

        self.ensure_db(&db, project)?;

        let oid = run_git(
            git()
                .arg("--git-dir")
                .arg(&db)
                .arg("rev-parse")
                .arg(format!("{}^{{commit}}", project.rev)),
        )
        .wrap_err_with(|| format!("`{}` is not a commit in {}", project.rev, project.repo.0))?;

        let dest = self.checkouts_dir(&ident).join(&oid[..16]);
        if dest.join(".ok").is_file() {
            return Ok(dest);
        }

        self.materialize(&db, &oid, &dest)?;
        Ok(dest)
    }

    fn ensure_db(&self, db: &Path, project: &Project) -> eyre::Result<()> {
        if !db.join("HEAD").is_file() {
            fs::create_dir_all(db).wrap_err("Failed to create database directory")?;
            run_git(git().args(["init", "--quiet", "--bare"]).arg(db))
                .wrap_err("Failed to initialize git repository")?;
        } else if has_commit(db, &project.rev) {
            return Ok(());
        }

        // A project pinned to a full commit hash names exactly the one object
        // worth having, so fetch only that, shallowly, instead of every
        // branch's entire history. GitHub and GitLab both serve an arbitrary
        // reachable commit this way; a host that refuses falls through to the
        // full fetch below.
        if project.is_full_sha() {
            let ref_name = format!("refs/decay/{}", project.rev);
            let shallow = git()
                .arg("--git-dir")
                .arg(db)
                .args(["fetch", "--depth", "1", "--quiet"])
                .arg(project.repo.0.to_string())
                .arg(format!("{}:{ref_name}", project.rev))
                .output();
            if shallow.is_ok_and(|out| out.status.success()) && has_commit(db, &project.rev) {
                return Ok(());
            }
        }

        run_git(
            git()
                .arg("--git-dir")
                .arg(db)
                .args(["fetch", "--force", "--tags", "--quiet"])
                .arg(project.repo.0.to_string())
                .arg("+refs/heads/*:refs/remotes/origin/*"),
        )
        .wrap_err_with(|| format!("Failed to fetch {}", project.repo.0))?;

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
