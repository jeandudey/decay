mod ast;
mod config;
mod eval;
mod git_cache;

use {
    crate::{
        ast::{eval, lower, raw::Node},
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
        OptionExt,
        bail, //
    },
    pyo3::{
        ffi::c_str,
        prelude::*, //
    },
    std::{
        env,
        ffi::CStr,
        fs,
        path::{
            Path,
            PathBuf, //
        },
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

static DUMPER: &CStr = c_str!(include_str!("dumper.py"));

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
                let code_block = lower(ast)?;
                println!("{code_block:?}");
                eval(&code_block)?;
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

fn parse(path: impl AsRef<Path>) -> eyre::Result<Node> {
    let json = Python::attach(|py| {
        let module = PyModule::from_code(py, DUMPER, c"dumper.py", c"dumper")
            .wrap_err("Failed to load dumper.py module")?;
        let json = module
            .getattr("dump")?
            .call1((path
                .as_ref()
                .to_str()
                .ok_or_eyre("Failed to convert path into a string")?,))?
            .extract::<String>()?;
        Ok::<_, eyre::Report>(json)
    })?;

    serde_json::from_str(&json).wrap_err("Failed to parse JSON AST")
}

fn cache_dir() -> eyre::Result<PathBuf> {
    let cache = env::var_os("XDG_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|v| v.join(".cache")))
        .wrap_err("Failed to find cache directory")?;
    Ok(cache.join("decay"))
}
