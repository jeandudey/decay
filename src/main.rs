use {
    crate::{
        config::{
            Config,
            Project, //
        },
        git_cache::GitCache,
        oracle::ConfigOracle,
        packages::Packages,
        sources::{
            CountingSources,
            DiskSources, //
        },
    },
    clap::{
        Parser,
        Subcommand, //
    },
    decay_build_ir::{
        Graph,
        Origin, //
    },
    decay_meson_logic::{
        Logic,
        Z3Solver, //
    },
    eyre::{
        Context,
        ContextCompat,
        bail, //
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
mod oracle;
mod packages;
mod sources;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// How many projects to evaluate at once. Independent projects (those whose
    /// `depends` are all already imported) run in parallel up to this many
    /// workers. Defaults to the number of CPUs, or `DECAY_JOBS` if set.
    #[arg(short = 'j', long, global = true)]
    jobs: Option<usize>,
}

#[derive(Subcommand)]
enum Command {
    /// Generate Buck build files
    Buckify,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,z3=off")),
        )
        .with_span_events(FmtSpan::CLOSE)
        .init();

    let jobs = resolve_jobs(cli.jobs);

    match cli.command {
        Command::Buckify => buckify(jobs),
    }
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
    let _ = jobs;
    let config = Config::from_file("decay.toml")?;

    let cache_dir = cache_dir()?;
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).wrap_err("Failed to create cache directory")?;
    }
    let git_cache = GitCache::new(&cache_dir);

    // Every project is executed before anything is written: the constraints
    // that come from meson rather than from a project are declared once, and
    // that set is only known once every project has been looked at. Projects
    // execute in the order `decay.toml` lists them, so what one of them
    // provides is known in time for a later one's `dependency()` to resolve
    // against it.
    let mut packages = Packages::default();
    let mut imported = Vec::new();
    for project in &config.projects {
        let done = execute(&git_cache, &config, project, &packages)?;
        packages.register(&done.package, &done.graph);
        imported.push(done);
    }

    let labels = decay_buck2::Labels {
        systems: config.systems.clone(),
        compilers: config.compilers.clone(),
        // What a sibling project provides answers a `dependency()` lookup the
        // same way an explicit entry would; an explicit one still overrides
        // it, for the rare case where it has to.
        dependencies: packages
            .targets()
            .chain(
                config
                    .dependencies
                    .iter()
                    .filter_map(|(name, dep)| Some((name.clone(), dep.target()?.to_owned()))),
            )
            .collect(),
        programs: config.programs.clone(),
    };

    let shared_dir = config.third_party_dir.join(SHARED_CONSTRAINTS);
    let shared = decay_buck2::Shared::collect(
        package_path(&shared_dir)?,
        imported.iter().map(|p| &p.graph),
    );

    // The projects are written first: a constraint nothing selects on is not
    // declared, and what selects on what is only known once they are generated.
    let mut generated = Vec::new();
    for project in &mut imported {
        let start = Instant::now();
        let build = decay_buck2::emit(
            &project.graph,
            &mut project.logic,
            &labels,
            &shared,
            &project.out,
            &project.package,
        )
        .wrap_err_with(|| format!("Failed to generate build files for `{}`", project.name))?;
        generated.push(build);
        info!(
            dir = %project.out.display(),
            emit_ms = start.elapsed().as_millis(),
            "wrote build files",
        );
    }

    shared.write(&labels, &decay_buck2::Used::everywhere(generated), &shared_dir)?;
    info!(dir = %shared_dir.display(), "wrote shared constraints");

    Ok(())
}

/// Where the constraints shared by every imported project live, relative to the
/// third-party directory.
const SHARED_CONSTRAINTS: &str = "constraints";

/// A project that has been executed and is waiting to be written out.
struct Imported {
    name: String,
    out: PathBuf,
    package: String,
    graph: Graph,
    logic: Logic<Z3Solver>,
}

fn execute(
    git_cache: &GitCache,
    config: &Config,
    project: &Project,
    packages: &Packages,
) -> eyre::Result<Imported> {
    let name = project.repo.short_name();

    if !project.is_full_sha() {
        bail!(
            "`{name}` is pinned to `{}`; the generated build fetches by commit hash, so \
             `rev` has to be one",
            project.rev
        );
    }

    let checkout_start = Instant::now();
    let dir = git_cache.checkout(project)?;
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
    graph.project.origin = Some(Origin {
        repo: project.repo.0.to_string(),
        rev: project.rev.clone(),
    });

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
        name,
        package: package_path(&out)?,
        out,
        graph,
        logic,
    })
}

/// A path as buck2 spells it in a label.
fn package_path(path: &Path) -> eyre::Result<String> {
    Ok(path
        .to_str()
        .wrap_err("The output directory is not valid UTF-8")?
        .trim_end_matches('/')
        .to_owned())
}

fn cache_dir() -> eyre::Result<PathBuf> {
    let cache = env::var_os("XDG_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|v| v.join(".cache")))
        .wrap_err("Failed to find cache directory")?;
    Ok(cache.join("decay"))
}
