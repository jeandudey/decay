use {
    decay_meson_ast::{
        Block,
        ProjectOptions, //
    },
    decay_meson_eval::Sources,
    std::path::Path,
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
}
