#![allow(dead_code)]

mod ast;
mod config;
mod git_cache;

use {
    crate::{
        ast::{
            eval,
            parse, //
        },
        config::Config,
        git_cache::GitCache,
    },
    clap::{
        Parser,
        Subcommand, //
    },
    eyre::{
        Context,
        ContextCompat,
        bail, //
    },
    std::{
        env,
        fs,
        path::PathBuf, //
    },
};

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
                let root_meson_file = project_dir.join("meson.build");
                if !root_meson_file.exists() {
                    bail!("Project does not contain a meson.build file");
                }

                let ast = parse(&root_meson_file).wrap_err("Failed to parse meson.build file")?;
                //println!("{ast:?}");
                eval(&ast)?;
                //let code_block = match ast {
                //    Node::CodeBlock(v) => v,
                //    _ => bail!("No root code block"),
                //};

                //for line in code_block.lines {
                //    match line {
                //        Node::Function(func) => {
                //            eval(func)?;
                //        }
                //        _ => bail!("Unhandled node: {line:?}"),
                //    }
                //}
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
