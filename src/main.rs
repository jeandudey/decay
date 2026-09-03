#![allow(dead_code, unused_variables)]

use {
    crate::{config::Config, git_cache::GitCache, oracle::Concrete},
    clap::{
        Parser,
        Subcommand, //
    },
    decay_meson_eval::eval,
    eyre::{
        Context,
        ContextCompat, //
    },
    std::{
        env,
        fs,
        path::PathBuf, //
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
        .with_env_filter(EnvFilter::new("trace,z3=off"))
        .with_span_events(FmtSpan::CLOSE)
        .init();

    match cli.command {
        Command::Buckify => {
            let config = Config::from_file("decay.toml")?;

            let cache_dir = cache_dir()?;
            if !cache_dir.exists() {
                fs::create_dir_all(&cache_dir).wrap_err("Failed to create cache directory")?;
            }

            let git_cache = GitCache::new(&cache_dir);

            for project in &config.projects {
                let project_dir = git_cache.checkout(&project)?;
                let ast = decay_meson_parse::parse_build(&project_dir.join("meson.build"))?;
                //let ir = decay_meson_ir::normalize(&ast);
                //info!(
                //    "{}",
                //    ir.iter()
                //        .map(|v| v.to_string())
                //        .collect::<Vec<_>>()
                //        .join("\n")
                //);
                let oracle = Concrete::new(&project.options, &project.host_machine);
                eval(&oracle, &ast)?;
                //eval(&project_dir, &config.systems)?;
            }
        }
    }

    Ok(())
}

fn cache_dir() -> eyre::Result<PathBuf> {
    let cache = env::var_os("XDG_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|v| v.join(".cache")))
        .wrap_err("Failed to find cache directory")?;
    Ok(cache.join("decay"))
}
