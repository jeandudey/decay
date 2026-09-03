use {
    decay_meson_ast::{
        Block,
        ProjectOptions, //
    },
    decay_meson_eval::Sources,
    eyre::Context,
    std::path::{
        Path,
        PathBuf, //
    },
};

/// Reads the project straight off disk, parsing with meson's own parser.
pub struct DiskSources;

impl Sources for DiskSources {
    fn build(&self, path: &Path) -> eyre::Result<Block> {
        decay_meson_parse::parse_build(path)
    }

    fn options(&self, dir: &Path) -> eyre::Result<Option<ProjectOptions>> {
        decay_meson_parse::parse_project_options(dir)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn read(&self, path: &Path) -> eyre::Result<String> {
        std::fs::read_to_string(path)
            .wrap_err_with(|| format!("Failed to read `{}`", path.display()))
    }

    fn list_dir(&self, dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_path_buf());
        }
    }
}
