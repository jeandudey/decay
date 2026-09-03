use {
    crate::{
        config::{
            Config,
            Project, //
        },
        git_cache::GitCache,
        oracle::ConfigOracle,
        sources::DiskSources, //
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
mod sources;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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

    match cli.command {
        Command::Buckify => buckify(),
    }
}

fn buckify() -> eyre::Result<()> {
    let config = Config::from_file("decay.toml")?;

    let cache_dir = cache_dir()?;
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).wrap_err("Failed to create cache directory")?;
    }
    let git_cache = GitCache::new(&cache_dir);

    let labels = decay_buck2::Labels {
        systems: config.systems.clone(),
        compilers: config.compilers.clone(),
        dependencies: config.dependencies.clone(),
    };

    // Every project is executed before anything is written: the constraints
    // that come from meson rather than from a project are declared once, and
    // that set is only known once every project has been looked at.
    let mut imported = Vec::new();
    for project in &config.projects {
        imported.push(execute(&git_cache, &config, project)?);
    }

    let shared_dir = config.third_party_dir.join(SHARED_CONSTRAINTS);
    let shared = decay_buck2::Shared::collect(
        package_path(&shared_dir)?,
        imported.iter().map(|p| &p.graph),
    );
    shared.write(&labels, &shared_dir)?;
    info!(dir = %shared_dir.display(), "wrote shared constraints");

    for project in &mut imported {
        decay_buck2::emit(
            &project.graph,
            &mut project.logic,
            &labels,
            &shared,
            &project.out,
            &project.package,
        )
        .wrap_err_with(|| format!("Failed to generate build files for `{}`", project.name))?;
        info!(dir = %project.out.display(), "wrote build files");
    }

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

fn execute(git_cache: &GitCache, config: &Config, project: &Project) -> eyre::Result<Imported> {
    let name = project.repo.short_name();

    if !project.is_full_sha() {
        bail!(
            "`{name}` is pinned to `{}`; the generated build fetches by commit hash, so \
             `rev` has to be one",
            project.rev
        );
    }

    let dir = git_cache.checkout(project)?;

    let oracle = ConfigOracle::new(config, project);
    let (mut graph, logic) = decay_meson_eval::eval(&oracle, &DiskSources, &dir)
        .wrap_err_with(|| format!("Failed to execute `{name}`"))?;

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
