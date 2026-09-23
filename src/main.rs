use {
    crate::{
        config::{
            Config,
            Project,
            Repo,
            Source, //
        },
        git_cache::GitCache,
        lock::Resolved,
        oracle::ConfigOracle,
        packages::Packages,
        sources::{
            CountingSources,
            DiskSources, //
        },
        wrap_cache::WrapCache,
        wrapdb::WrapSource,
    },
    clap::Parser,
    decay_build_ir::{
        ArchiveFile,
        Graph,
        Origin,
        WrapdbOverlay, //
    },
    decay_meson_logic::{
        Logic,
        VarKind,
        Z3Solver, //
    },
    eyre::{
        Context,
        ContextCompat, //
    },
    std::{
        env,
        fs,
        path::{
            Path,
            PathBuf, //
        },
        thread,
        time::Instant, //
    },
    tracing::{
        info,
        warn, //
    },
    tracing_subscriber::{
        EnvFilter,
        fmt::format::FmtSpan, //
    },
    url::Url,
};

mod config;
mod git_cache;
mod lock;
mod oracle;
mod packages;
mod pool;
mod probe;
mod run_command;
mod schedule;
mod sources;
mod wrap_cache;
mod wrapdb;

/// Generate Buck build files
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// How many projects to evaluate at once. Independent projects (those whose
    /// `depends` are all already imported) run in parallel up to this many
    /// workers. Defaults to the number of CPUs, or `DECAY_JOBS` if set.
    #[arg(short = 'j', long, global = true)]
    jobs: Option<usize>,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,z3=off")),
        )
        .with_span_events(FmtSpan::CLOSE)
        .init();

    buckify(resolve_jobs(cli.jobs))
}

/// Worker count: `--jobs`, else `DECAY_JOBS`, else the CPU count, else 1. Always
/// at least 1.
fn resolve_jobs(flag: Option<usize>) -> usize {
    flag.or_else(|| env::var("DECAY_JOBS").ok().and_then(|v| v.parse().ok()))
        .or_else(|| thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
        .max(1)
}

fn buckify(jobs: usize) -> eyre::Result<()> {
    let config = Config::from_file("decay.toml")?;

    let cache_dir = cache_dir()?;
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).wrap_err("Failed to create cache directory")?;
    }
    let git_cache = GitCache::new(&cache_dir);
    let wrap_cache = WrapCache::new(&cache_dir);

    // A moving git `branch`, `tag`, or `rev`, or a wrap left to resolve against
    // wrapdb's latest, is pinned here — once per run, before anything is
    // scheduled — rather than repeated by every worker that happens to
    // evaluate that entry.
    let resolved = lock::resolve(
        Path::new("decay.lock"),
        &config.projects,
        &git_cache,
        config.wrap_dir.as_deref(),
    )
    .wrap_err("Failed to resolve decay.lock")?;

    // Projects run one wave at a time, so what one provides is known in time
    // for a later one's `dependency()` to resolve against it. A project whose
    // `depends` are all in earlier waves — a project with no `depends` among
    // them — runs alongside its wave-mates, up to `-j` at once.
    let schedule = schedule::plan(&config.projects)?;
    info!(
        projects = config.projects.len(),
        waves = schedule.waves.len(),
        jobs,
        "importing",
    );

    pool::import(&config, &git_cache, &wrap_cache, &resolved, &schedule, jobs)
}

/// Where the constraints shared by every imported project live, relative to the
/// third-party directory.
pub(crate) const SHARED_CONSTRAINTS: &str = "constraints";

/// A project that has been executed and is waiting to be written out.
pub(crate) struct Imported {
    out: PathBuf,
    package: String,
    graph: Graph,
    logic: Logic<Z3Solver>,
}

pub(crate) fn execute(
    git_cache: &GitCache,
    wrap_cache: &WrapCache,
    config: &Config,
    project: &Project,
    resolved: &Resolved,
    packages: &Packages,
) -> eyre::Result<Imported> {
    let name = project.short_name();

    let checkout_start = Instant::now();
    let (dir, origin, wrapdb_overlay) = match (&project.source, resolved) {
        (Source::Git { repo, .. }, Resolved::Git { rev }) => {
            let dir = git_cache.checkout(repo, rev)?;
            let origin = Origin::Git {
                repo: repo.0.to_string(),
                rev: rev.clone(),
            };
            (dir, origin, None)
        }
        (Source::Wrap { .. }, Resolved::Wrap { version, file }) => match &file.source {
            WrapSource::Archive { .. } => {
                let wrap = wrap_cache.materialize(git_cache, &name, version, file)?;
                let origin = Origin::Archive(ArchiveFile {
                    url: wrap.url,
                    sha256: wrap.sha256,
                    strip_prefix: wrap.strip_prefix,
                });
                // `overlay_paths` is only ever non-empty when the wrap named
                // a `patch_directory` and it came from wrapdb, not a
                // `local_overlay` test fixture (`WrapCache::materialize`).
                let wrapdb_overlay = (!wrap.overlay_paths.is_empty()).then(|| WrapdbOverlay {
                    rev: file.wrapdb_rev.clone(),
                    patch_directory: file
                        .patch_directory
                        .clone()
                        .expect("non-empty overlay_paths implies patch_directory"),
                    paths: wrap.overlay_paths,
                });
                (wrap.dir, origin, wrapdb_overlay)
            }
            WrapSource::Git { url, revision } => {
                let repo = Repo(Url::parse(url).wrap_err_with(|| format!("`{url}` is not a URL"))?);
                let checkout = git_cache.checkout(&repo, revision)?;
                let overlay =
                    file.patch_directory
                        .as_deref()
                        .map(|dir| {
                            file.local_overlay.clone().map(Ok).unwrap_or_else(|| {
                                wrapdb::patch_dir(git_cache, &file.wrapdb_rev, dir)
                            })
                        })
                        .transpose()?;
                let dir =
                    wrap_cache.materialize_git(&name, version, &checkout, overlay.as_deref())?;
                let origin = Origin::Git {
                    repo: url.clone(),
                    rev: revision.clone(),
                };
                // Same `local_overlay` exception as the `[wrap-file]` arm
                // above: only a real wrapdb-sourced overlay gets a
                // `WrapdbOverlay` (a `local_overlay` has no stable commit a
                // generated build could fetch it from).
                let wrapdb_overlay = match (&overlay, &file.local_overlay, &file.patch_directory) {
                    (Some(dir), None, Some(patch_directory)) => Some(WrapdbOverlay {
                        rev: file.wrapdb_rev.clone(),
                        patch_directory: patch_directory.clone(),
                        paths: wrap_cache::list(dir)?,
                    }),
                    _ => None,
                };
                (dir, origin, wrapdb_overlay)
            }
        },
        _ => {
            unreachable!("`lock::resolve` produces one `Resolved` per `Source`, in the same order")
        }
    };
    let checkout_ms = checkout_start.elapsed().as_millis();

    let oracle = ConfigOracle::new(config, project, packages, &dir);
    let sources = CountingSources::new(&DiskSources);
    let eval_start = Instant::now();
    let (mut graph, mut logic) = decay_meson_eval::eval(&oracle, &sources, &dir)
        .wrap_err_with(|| format!("Failed to execute `{name}`"))?;
    let eval_ms = eval_start.elapsed().as_millis();
    let parse_ms = sources.parse_time().as_millis();

    // The build files fetch the sources themselves rather than referring to a
    // copy of them checked into this repository.
    graph.project.origin = Some(origin);
    graph.project.wrapdb_overlay = wrapdb_overlay;

    // The fetch target's own name is only known now — a real target claimed
    // during evaluation just above can happen to share it (a `library()`
    // named after its own project, as fribidi's is).
    let repo_target = graph.project.repo_target();
    graph.avoid_name_collision(&repo_target);
    if graph.project.wrapdb_overlay.is_some() {
        let wrapdb_target = graph.project.wrapdb_target();
        graph.avoid_name_collision(&wrapdb_target);
    }

    // What a sibling's `dependency()` will read back. Only meson's own
    // variables mean the same thing in another project; this project's own
    // options do not exist there, so a provide gated on one counts as present
    // wherever some setting of it would provide it.
    for provide in &mut graph.provides {
        let (found, dropped) = logic
            .arena_mut()
            .export(provide.cond, |var| var.kind != VarKind::Option);
        if !dropped.is_empty() {
            warn!(
                project = %graph.project.name,
                provide = %provide.name,
                options = ?dropped,
                "provided only under some settings of this project's own options; \
                 a sibling's `dependency()` treats it as present under all of them",
            );
        }
        provide.found = found;
    }

    info!(
        project = %graph.project.name,
        targets = graph.targets.len(),
        tests = graph.tests.len(),
        open_options = graph.options.len(),
        checkout_ms,
        parse_ms,
        parse_calls = sources.parse_calls(),
        interp_ms = eval_ms.saturating_sub(parse_ms),
        eval_ms,
        "executed",
    );

    let out = config.third_party_dir.join(&name);
    Ok(Imported {
        package: package_path(&out)?,
        out,
        graph,
        logic,
    })
}

/// A path as buck2 spells it in a label.
pub(crate) fn package_path(path: &Path) -> eyre::Result<String> {
    Ok(path
        .to_str()
        .wrap_err("The output directory is not valid UTF-8")?
        .trim_end_matches('/')
        .to_owned())
}

fn cache_dir() -> eyre::Result<PathBuf> {
    let cache = platform_cache_dir().wrap_err("Failed to find cache directory")?;
    Ok(cache.join("decay"))
}

/// Where a platform keeps a user's cache: `%LOCALAPPDATA%` on Windows,
/// `~/Library/Caches` on macOS, and `$XDG_CACHE_HOME` (falling back to
/// `~/.cache`, per the XDG Base Directory spec) everywhere else.
#[cfg(target_os = "windows")]
fn platform_cache_dir() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA").map(PathBuf::from).or_else(|| {
        env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("AppData").join("Local"))
    })
}

#[cfg(target_os = "macos")]
fn platform_cache_dir() -> Option<PathBuf> {
    env::home_dir().map(|home| home.join("Library").join("Caches"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_cache_dir() -> Option<PathBuf> {
    env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|home| home.join(".cache")))
}
