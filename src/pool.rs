//! Running the projects on a scoped pool, one thread per project for that
//! project's whole life.
//!
//! `Logic<Z3Solver>` is not `Send` — z3 keeps its context in a thread-local —
//! and the same `Logic` is built during evaluation and mutated again during
//! emission. So a project is evaluated *and* emitted on the one worker that
//! owns it; only its `Graph` (plain data) and the emitted build file text ever
//! move between threads.
//!
//! The coordinator ([`import`]) hands work out wave by wave, folds each wave's
//! results into [`Packages`] in project order, crosses the single barrier where
//! the shared constraints are gathered from every graph, then sends each
//! project back to its owner for emission.

use {
    crate::{
        Imported,
        SHARED_CONSTRAINTS,
        config::{
            Config,
            Project, //
        },
        execute,
        git_cache::GitCache,
        lock::Resolved,
        package_path,
        packages::Packages,
        schedule::Schedule,
        wrap_cache::WrapCache,
    },
    decay_buck2::{
        Labels,
        Shared,
        Used, //
    },
    decay_build_ir::Graph,
    decay_meson_logic::{
        Logic,
        Z3Solver, //
    },
    eyre::{
        Context,
        eyre, //
    },
    std::{
        collections::HashMap,
        fs,
        panic::{
            AssertUnwindSafe,
            catch_unwind, //
        },
        path::PathBuf,
        sync::{
            Arc,
            mpsc, //
        },
        thread,
        time::Instant, //
    },
    tracing::info,
};

/// What a worker sends back once it has evaluated a project: the graph travels
/// to the coordinator, everything else (the `!Send` logic included) stays on
/// the worker as [`Pinned`].
struct Evaled {
    idx: usize,
    package: String,
    graph: Graph,
}

/// The worker-local half of an evaluated project, kept until it is emitted.
struct Pinned {
    out: PathBuf,
    package: String,
    logic: Logic<Z3Solver>,
}

enum Job {
    Eval {
        idx: usize,
        packages: Arc<Packages>,
    },
    Emit {
        idx: usize,
        // Boxed: `Job` travels down an mpsc channel one message at a time, and
        // a bare `Graph` would otherwise make every `Job` (including the tiny
        // `Eval`/`Stop` ones) as large as the biggest project's graph.
        graph: Box<Graph>,
        labels: Arc<Labels>,
        shared: Arc<Shared>,
    },
    Stop,
}

enum Done {
    Evaled {
        idx: usize,
        // Boxed for the same reason as `Job::Emit`'s `graph`: keeps the common,
        // small `Done` variants from paying for the rare large one.
        result: Box<eyre::Result<Evaled>>,
    },
    Emitted {
        idx: usize,
        name: String,
        out: PathBuf,
        emit_ms: u128,
        result: eyre::Result<String>,
    },
}

/// Import every project in `config`, following `schedule`'s waves, on up to
/// `jobs` worker threads. Writes every project's build files and the shared
/// constraints package as a side effect.
pub(crate) fn import(
    config: &Config,
    git_cache: &GitCache,
    wrap_cache: &WrapCache,
    resolved: &[Resolved],
    schedule: &Schedule,
    jobs: usize,
) -> eyre::Result<()> {
    let workers = jobs.max(1);
    let projects = &config.projects;
    let total = projects.len();
    let owner = |idx: usize| idx % workers;

    // Bring the embedded Python interpreter and the meson parser modules up on
    // this thread before any worker touches them: the first import is not safe
    // to race.
    decay_meson_parse::warmup()?;

    thread::scope(|scope| -> eyre::Result<()> {
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        let mut inbox: Vec<mpsc::Sender<Job>> = Vec::with_capacity(workers);
        for _ in 0..workers {
            let (job_tx, job_rx) = mpsc::channel::<Job>();
            inbox.push(job_tx);
            let done_tx = done_tx.clone();
            scope.spawn(move || {
                worker(git_cache, wrap_cache, config, projects, resolved, job_rx, done_tx)
            });
        }
        drop(done_tx);

        // Tell every worker to finish, so `thread::scope` can join them rather
        // than block on one still parked in `recv`. Called before every early
        // return.
        let stop = |inbox: &[mpsc::Sender<Job>]| {
            for tx in inbox {
                let _ = tx.send(Job::Stop);
            }
        };

        let mut packages = Packages::default();
        let mut graphs: Vec<Option<Graph>> = (0..total).map(|_| None).collect();

        // --- evaluate, wave by wave ---
        for wave in &schedule.waves {
            let snapshot = Arc::new(packages.clone());
            for &idx in wave {
                let _ = inbox[owner(idx)].send(Job::Eval {
                    idx,
                    packages: snapshot.clone(),
                });
            }

            let mut evaled: Vec<Evaled> = Vec::with_capacity(wave.len());
            let mut failed: Vec<(usize, eyre::Report)> = Vec::new();
            for _ in 0..wave.len() {
                match done_rx.recv() {
                    Ok(Done::Evaled { idx, result }) => match *result {
                        Ok(e) => evaled.push(e),
                        Err(err) => failed.push((idx, err)),
                    },
                    Ok(Done::Emitted { .. }) => unreachable!("no emit dispatched yet"),
                    Err(_) => {
                        stop(&inbox);
                        return Err(eyre!("a worker thread stopped unexpectedly"));
                    }
                }
            }
            if !failed.is_empty() {
                stop(&inbox);
                failed.sort_by_key(|(idx, _)| *idx);
                let (idx, err) = failed.into_iter().next().unwrap();
                return Err(err.wrap_err(format!(
                    "Failed to import `{}`",
                    projects[idx].short_name()
                )));
            }

            // Fold results in project order so what `Packages` holds after a
            // wave depends only on which projects it contained, not on the
            // order they happened to finish.
            evaled.sort_by_key(|e| e.idx);
            for e in evaled {
                packages.register(&e.package, &e.graph);
                graphs[e.idx] = Some(e.graph);
            }
        }

        // Every project evaluated cleanly, so we know something will be
        // written: only now is it safe to clean the directory. Wiping it
        // earlier — before evaluation could still fail — would trade a stale
        // leftover for an empty directory on every failed run.
        if config.third_party_dir.exists() {
            fs::remove_dir_all(&config.third_party_dir)
                .wrap_err("Failed to clean the third-party directory")?;
        }

        // --- barrier: the shared constraints need every graph ---
        let labels = Arc::new(build_labels(config, &packages));
        let shared_dir = config.third_party_dir.join(SHARED_CONSTRAINTS);
        let shared = Arc::new(Shared::collect(
            package_path(&shared_dir)?,
            graphs.iter().map(|g| g.as_ref().expect("every project evaluated")),
        ));

        // --- emit, each project back on the worker that evaluated it ---
        for idx in 0..total {
            let graph = graphs[idx].take().expect("every project evaluated");
            let _ = inbox[owner(idx)].send(Job::Emit {
                idx,
                graph: Box::new(graph),
                labels: labels.clone(),
                shared: shared.clone(),
            });
        }

        let mut files: Vec<Option<String>> = (0..total).map(|_| None).collect();
        let mut failed: Vec<(usize, eyre::Report)> = Vec::new();
        for _ in 0..total {
            match done_rx.recv() {
                Ok(Done::Emitted {
                    idx,
                    name,
                    out,
                    emit_ms,
                    result,
                }) => match result {
                    Ok(text) => {
                        info!(dir = %out.display(), emit_ms, "wrote build files");
                        files[idx] = Some(text);
                    }
                    Err(err) => failed.push((
                        idx,
                        err.wrap_err(format!("Failed to generate build files for `{name}`")),
                    )),
                },
                Ok(Done::Evaled { .. }) => unreachable!("evaluation is done"),
                Err(_) => {
                    stop(&inbox);
                    return Err(eyre!("a worker thread stopped unexpectedly"));
                }
            }
        }

        stop(&inbox);

        if !failed.is_empty() {
            failed.sort_by_key(|(idx, _)| *idx);
            return Err(failed.into_iter().next().unwrap().1);
        }

        let files: Vec<String> = files
            .into_iter()
            .map(|f| f.expect("every project emitted"))
            .collect();
        shared
            .write(&labels, &Used::everywhere(files), &shared_dir)
            .wrap_err("Failed to write shared constraints")?;
        info!(dir = %shared_dir.display(), "wrote shared constraints");

        Ok(())
    })
}

fn worker(
    git_cache: &GitCache,
    wrap_cache: &WrapCache,
    config: &Config,
    projects: &[Project],
    resolved: &[Resolved],
    jobs: mpsc::Receiver<Job>,
    done: mpsc::Sender<Done>,
) {
    // This worker's evaluated-but-not-yet-emitted projects. Holds the `!Send`
    // `Logic`, so it must never leave this thread.
    let mut pinned: HashMap<usize, Pinned> = HashMap::new();

    while let Ok(job) = jobs.recv() {
        let msg = match job {
            Job::Stop => break,

            Job::Eval { idx, packages } => {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    execute(git_cache, wrap_cache, config, &projects[idx], &resolved[idx], &packages)
                }));
                match flatten(outcome) {
                    Ok(Imported {
                        out,
                        package,
                        graph,
                        logic,
                    }) => {
                        pinned.insert(idx, Pinned {
                            out,
                            package: package.clone(),
                            logic,
                        });
                        Done::Evaled {
                            idx,
                            result: Box::new(Ok(Evaled {
                                idx,
                                package,
                                graph,
                            })),
                        }
                    }
                    Err(err) => Done::Evaled {
                        idx,
                        result: Box::new(Err(err)),
                    },
                }
            }

            Job::Emit {
                idx,
                graph,
                labels,
                shared,
            } => {
                let Pinned {
                    out,
                    package,
                    mut logic,
                } = pinned
                    .remove(&idx)
                    .expect("emit for a project this worker never evaluated");
                let name = projects[idx].short_name();
                let start = Instant::now();
                let result = flatten(catch_unwind(AssertUnwindSafe(|| {
                    decay_buck2::emit(&graph, &mut logic, &labels, &shared, &out, &package)
                })));
                Done::Emitted {
                    idx,
                    name,
                    out,
                    emit_ms: start.elapsed().as_millis(),
                    result,
                }
            }
        };
        if done.send(msg).is_err() {
            return;
        }
    }
}

/// Collapse a caught panic into the error channel so the coordinator always
/// gets exactly one reply per job.
fn flatten<T>(outcome: thread::Result<eyre::Result<T>>) -> eyre::Result<T> {
    match outcome {
        Ok(result) => result,
        Err(panic) => {
            let what = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_owned());
            Err(eyre!("panicked: {what}"))
        }
    }
}

/// The labels the emitter is handed for choices it must not invent constraints
/// for: what `decay.toml` names, plus what a sibling project already provides
/// (an explicit `[dependencies]` entry still wins).
fn build_labels(config: &Config, packages: &Packages) -> Labels {
    Labels {
        systems: config.systems.clone(),
        compilers: config.compilers.clone(),
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
    }
}
