use {
    crate::{
        config::{
            Config,
            Project,
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
    },
    clap::Parser,
    decay_build_ir::{
        ArchiveFile,
        Graph,
        Origin, //
    },
    decay_meson_logic::{
        Logic,
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
    tracing::info,
    tracing_subscriber::{
        EnvFilter,
        fmt::format::FmtSpan, //
    },
};

mod config;
mod git_cache;
mod lock;
mod oracle;
mod packages;
mod pool;
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

    // The directory is wholly ours: wipe it before regenerating so it ends up
    // holding exactly what `decay.toml` produces, not leftovers from a project
    // that used to be imported or a file nothing here writes.
    if config.third_party_dir.exists() {
        fs::remove_dir_all(&config.third_party_dir)
            .wrap_err("Failed to clean the third-party directory")?;
    }

    let cache_dir = cache_dir()?;
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).wrap_err("Failed to create cache directory")?;
    }
    let git_cache = GitCache::new(&cache_dir);
    let wrap_cache = WrapCache::new(&cache_dir);

    // A `rev` that names a branch or tag, or a wrap left to resolve against
    // wrapdb's latest, is pinned here — once per run, before anything is
    // scheduled — rather than repeated by every worker that happens to
    // evaluate that entry.
    let resolved = lock::resolve(Path::new("decay.lock"), &config.projects)
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
    let (dir, origin) = match (&project.source, resolved) {
        (Source::Git { repo, .. }, Resolved::Git { rev }) => {
            let dir = git_cache.checkout(repo, rev)?;
            let origin = Origin::Git { repo: repo.0.to_string(), rev: rev.clone() };
            (dir, origin)
        }
        (Source::Wrap { .. }, Resolved::Wrap { version, file }) => {
            let wrap = wrap_cache.materialize(&name, version, file)?;
            let origin = Origin::Archive(ArchiveFile {
                url: wrap.url,
                sha256: wrap.sha256,
                strip_prefix: wrap.strip_prefix,
            });
            (wrap.dir, origin)
        }
        _ => unreachable!("`lock::resolve` produces one `Resolved` per `Source`, in the same order"),
    };
    let checkout_ms = checkout_start.elapsed().as_millis();

    let oracle = ConfigOracle::new(config, project, packages);
    let sources = CountingSources::new(&DiskSources);
    let eval_start = Instant::now();
    let (mut graph, logic) = decay_meson_eval::eval(&oracle, &sources, &dir)
        .wrap_err_with(|| format!("Failed to execute `{name}`"))?;
    let eval_ms = eval_start.elapsed().as_millis();
    let parse_ms = sources.parse_time().as_millis();

    // The build files fetch the sources themselves rather than referring to a
    // copy of them checked into this repository.
    graph.project.origin = Some(origin);

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
