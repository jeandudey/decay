use {
    decay_meson_ast::{
        Block,
        ProjectOptions, //
    },
    decay_meson_eval::Sources,
    eyre::Context,
    std::{
        path::{
            Path,
            PathBuf, //
        },
        sync::atomic::{
            AtomicU64,
            Ordering, //
        },
        time::{
            Duration,
            Instant, //
        },
    },
};

/// Reads the project straight off disk, parsing with meson's own parser.
pub struct DiskSources;

/// Wraps a [`Sources`] and records how long parsing takes, so the driver can
/// report the parse / interpret split per project without the executor knowing
/// anything about timing.
pub struct CountingSources<'a> {
    inner: &'a dyn Sources,
    parse_nanos: AtomicU64,
    parse_calls: AtomicU64,
}

impl<'a> CountingSources<'a> {
    pub fn new(inner: &'a dyn Sources) -> Self {
        Self {
            inner,
            parse_nanos: AtomicU64::new(0),
            parse_calls: AtomicU64::new(0),
        }
    }

    /// Cumulative time spent in `build` and `options`.
    pub fn parse_time(&self) -> Duration {
        Duration::from_nanos(self.parse_nanos.load(Ordering::Relaxed))
    }

    /// How many meson files were parsed.
    pub fn parse_calls(&self) -> u64 {
        self.parse_calls.load(Ordering::Relaxed)
    }

    fn timed<T>(&self, f: impl FnOnce() -> eyre::Result<T>) -> eyre::Result<T> {
        let start = Instant::now();
        let out = f();
        self.parse_nanos
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        self.parse_calls.fetch_add(1, Ordering::Relaxed);
        out
    }
}

impl Sources for CountingSources<'_> {
    fn build(&self, path: &Path) -> eyre::Result<Block> {
        self.timed(|| self.inner.build(path))
    }

    fn options(&self, dir: &Path) -> eyre::Result<Option<ProjectOptions>> {
        self.timed(|| self.inner.options(dir))
    }

    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.inner.is_file(path)
    }

    fn read(&self, path: &Path) -> eyre::Result<String> {
        self.inner.read(path)
    }

    fn list_dir(&self, dir: &Path) -> Vec<PathBuf> {
        self.inner.list_dir(dir)
    }
}

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
