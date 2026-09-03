//! A variational executor for the meson build definition language.
//!
//! Instead of running a `meson.build` once for one configuration, the executor
//! runs it once for *all* configurations: values carry the set of
//! configurations they hold in ([`Variational`]), and control flow narrows a
//! path condition rather than picking a branch. Whatever the [`Oracle`] does
//! not pin down stays open, so a project that lets you turn GLX on and off
//! produces a build graph that still lets you turn GLX on and off.

use {
    crate::{
        args::CallArgs,
        obj::{
            ConfigData,
            Dep,
            Machine,
            Obj,
            Program, //
        },
        oracle::Oracle,
        val::Value,
    },
    decay_build_ir::{
        External,
        Graph,
        Kind,
        Source,
        TargetId, //
    },
    decay_meson_ast::{
        AssignStmt,
        Block,
        Expr,
        ForeachStmt,
        IfStmt,
        Loc,
        ProjectOption,
        ProjectOptions,
        Stmt,
        Ternary,
        UnOp,
        UnOpKind, //
    },
    decay_meson_logic::{
        Logic,
        Pc,
        Solver,
        Var,
        VarId,
        VarKind,
        Variant,
        Variational, //
    },
    eyre::{
        Context,
        bail, //
    },
    std::{
        cell::RefCell,
        collections::{
            HashMap,
            HashSet, //
        },
        mem,
        path::{
            Path,
            PathBuf, //
        },
        rc::Rc,
    },
    tracing::{
        trace,
        warn, //
    },
};

mod args;
mod builtins;
mod methods;
mod ops;
mod strings;

pub mod obj;
pub mod oracle;
pub mod val;

/// How the executor gets at the project's sources.
///
/// Parsing lives behind a trait so the executor itself stays free of the Python
/// meson parser, and so tests can feed it sources directly.
pub trait Sources {
    /// Parse the `meson.build` at `path`.
    fn build(&self, path: &Path) -> eyre::Result<Block>;

    /// Parse the option declarations for the project rooted at `dir`, if it
    /// declares any.
    fn options(&self, dir: &Path) -> eyre::Result<Option<ProjectOptions>>;

    /// Whether a path exists in the source tree.
    fn exists(&self, path: &Path) -> bool;

    /// Expand a glob-free listing of a directory, used by `files()` sanity
    /// checks. Returning `None` means "do not check".
    fn is_file(&self, path: &Path) -> bool {
        self.exists(path)
    }

    /// The text content of a file in the source tree, for `fs.read()`.
    ///
    /// Reading straight from the pinned checkout is faithful here in a way it
    /// would not be for a compiled artifact: the content at this commit is
    /// exactly what a real configure run would have read too.
    fn read(&self, path: &Path) -> eyre::Result<String>;

    /// Every file under `dir` in the source tree, recursively, as paths
    /// relative to it.
    ///
    /// Meson gives a compiled file free quote-include access to everything
    /// under one of its `include_directories()`, because `-I` is real
    /// filesystem access; a project rarely lists such a header explicitly.
    /// Listing straight from the pinned checkout is how the importer answers
    /// that without needing the project to.
    fn list_dir(&self, dir: &Path) -> Vec<PathBuf>;
}

/// Execute the project rooted at `root` and return its build graph, paired with
/// the presence-condition logic it was built against. The solver backend is the
/// caller's: [`decay_meson_logic::BddSolver`] or [`decay_meson_logic::Z3Solver`].
pub fn eval<S: Solver + Default>(
    oracle: &dyn Oracle,
    sources: &dyn Sources,
    root: &Path,
) -> eyre::Result<(Graph, Logic<S>)> {
    let mut interp = Interp::new(S::default(), oracle, sources, root);
    interp.run()?;
    Ok(interp.finish())
}

/// Statement-level control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Normal,
    Break,
    Continue,
    /// `error()` was reached: this path does not configure at all.
    Abort,
}

pub struct Interp<'a, S: Solver> {
    pub(crate) logic: Logic<S>,
    pub(crate) oracle: &'a dyn Oracle,
    pub(crate) sources: &'a dyn Sources,

    /// The configurations the statement being executed applies to.
    pub(crate) pc: Pc,
    flow: Flow,
    pub(crate) vars: HashMap<String, Variational<Value>>,

    pub(crate) graph: Graph,

    /// Option declarations from `meson.options` / `meson_options.txt`.
    pub(crate) options: ProjectOptions,
    /// `default_options:` from the `project()` call.
    pub(crate) default_options: HashMap<String, Rc<str>>,
    pub(crate) has_project: bool,

    pub(crate) root: PathBuf,
    /// Directory stack, relative to the project root.
    dirs: Vec<PathBuf>,
    visited: HashSet<PathBuf>,

    /// Interned configuration variables, so a second `get_option('glx')` is the
    /// same variable as the first.
    option_vars: HashMap<String, VarId>,
    probe_vars: HashMap<String, VarId>,
    /// Interned external dependency targets, keyed the same way.
    externals: HashMap<String, TargetId>,

    /// `add_project_arguments()` / `add_global_arguments()`: meson applies
    /// these to every target the project compiles, regardless of whether the
    /// call comes before or after a given `library()`/`executable()`, so they
    /// are collected here and only merged into every compiled target once the
    /// whole project has run, in [`Self::finish`].
    project_args: Variational<String>,
    /// The same, for `add_project_link_arguments()` / `add_global_link_arguments()`.
    project_link_args: Variational<String>,
}

impl<'a, S: Solver> Interp<'a, S> {
    pub fn new(solver: S, oracle: &'a dyn Oracle, sources: &'a dyn Sources, root: &Path) -> Self {
        let mut vars = HashMap::new();
        for (name, obj) in [
            ("meson", Obj::Meson),
            ("host_machine", Obj::Machine(Machine::Host)),
            ("build_machine", Obj::Machine(Machine::Build)),
            ("target_machine", Obj::Machine(Machine::Target)),
        ] {
            vars.insert(name.to_owned(), Variational::pure(Value::Obj(obj)));
        }

        Self {
            logic: Logic::new(solver),
            oracle,
            sources,
            pc: Pc::TRUE,
            flow: Flow::Normal,
            vars,
            graph: Graph::new(),
            options: ProjectOptions::new(),
            default_options: HashMap::new(),
            has_project: false,
            root: root.to_path_buf(),
            dirs: Vec::new(),
            visited: HashSet::new(),
            option_vars: HashMap::new(),
            probe_vars: HashMap::new(),
            externals: HashMap::new(),
            project_args: Variational::empty(),
            project_link_args: Variational::empty(),
        }
    }

    pub fn run(&mut self) -> eyre::Result<()> {
        if let Some(options) = self.sources.options(&self.root)? {
            self.options = options;
        }
        self.subdir(Path::new(""))
    }

    /// The finished graph, together with the logic it was built against.
    ///
    /// A backend needs the logic, not just the list of variables: rendering a
    /// presence condition means taking it apart, and deciding whether one is
    /// worth rendering at all means asking whether it can differ from its
    /// context.
    pub fn finish(self) -> (Graph, Logic<S>) {
        let mut graph = self.graph;
        graph.options = self.logic.vars().to_vec();

        // `add_project_arguments()` reaches every target the project
        // compiles, not just ones declared after the call, so it is applied
        // here rather than at the call site.
        if !self.project_args.is_empty() || !self.project_link_args.is_empty() {
            for target in &mut graph.targets {
                if !matches!(
                    target.kind,
                    Kind::StaticLibrary | Kind::SharedLibrary | Kind::Library { .. } | Kind::Executable
                ) {
                    continue;
                }
                let mut args = self.project_args.clone();
                args.extend(std::mem::take(&mut target.attrs.compile_args));
                target.attrs.compile_args = args;

                let mut link_args = self.project_link_args.clone();
                link_args.extend(std::mem::take(&mut target.attrs.link_args));
                target.attrs.link_args = link_args;
            }
        }

        (graph, self.logic)
    }

    pub fn logic_mut(&mut self) -> &mut Logic<S> {
        &mut self.logic
    }

    // -- source tree ------------------------------------------------------

    /// The directory of the `meson.build` currently executing, relative to the
    /// project root.
    pub(crate) fn cur_dir(&self) -> &Path {
        self.dirs.last().map(PathBuf::as_path).unwrap_or(Path::new(""))
    }

    /// Resolve a path written in the current `meson.build` against the project
    /// root, which is how every path reaches the build graph.
    pub(crate) fn resolve(&self, path: &str) -> String {
        let joined = self.cur_dir().join(path);
        normalize_path(&joined)
    }

    pub(crate) fn subdir(&mut self, dir: &Path) -> eyre::Result<()> {
        let abs = self.root.join(dir).join("meson.build");
        if !self.sources.exists(&abs) {
            bail!("no meson.build in `{}`", dir.display());
        }
        if !self.visited.insert(dir.to_path_buf()) {
            bail!("`{}` was entered twice", dir.display());
        }

        let block = self
            .sources
            .build(&abs)
            .wrap_err_with(|| format!("in {}", abs.display()))?;

        self.dirs.push(dir.to_path_buf());
        let saved_flow = mem::replace(&mut self.flow, Flow::Normal);
        let r = self.block(&block);
        self.flow = saved_flow;
        self.dirs.pop();
        r.wrap_err_with(|| format!("in {}/meson.build", dir.display()))
    }

    // -- statements -------------------------------------------------------

    pub fn block(&mut self, block: &Block) -> eyre::Result<()> {
        for stmt in &block.0 {
            self.stmt(stmt)?;
            if self.flow != Flow::Normal {
                break;
            }
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> eyre::Result<()> {
        match stmt {
            Stmt::Expr(v) => {
                self.expr(v)?;
            }
            Stmt::Assign(AssignStmt { name, val, is_plus }) => {
                let val = self.expr(val)?;
                if *is_plus {
                    self.plus_assign(name, &val)?;
                } else {
                    self.assign(name, val);
                }
            }
            Stmt::If(v) => self.exec_if(v)?,
            Stmt::Foreach(v) => self.exec_foreach(v)?,
            Stmt::Break => self.flow = Flow::Break,
            Stmt::Continue => self.flow = Flow::Continue,
        }
        Ok(())
    }

    /// Run each arm under the configurations that reach it.
    ///
    /// There is no merge step: an assignment already writes itself as "under
    /// this condition the new value, otherwise the old one", so the branches
    /// converge on their own.
    fn exec_if(&mut self, stmt: &IfStmt) -> eyre::Result<()> {
        let entry = self.pc;
        // Configurations that have fallen through every arm so far.
        let mut open = entry;

        for (cond, block) in &stmt.arms {
            if open.is_false() {
                break;
            }
            let value = self.with_pc(open, |this| this.expr(cond))?;
            let taken = self.truth(&value)?;
            let taken = self.logic.and(open, taken);
            let not_taken = self.logic.not(taken);
            open = self.logic.and(open, not_taken);

            if taken.is_false() || !self.logic.is_sat(taken) {
                continue;
            }
            self.run_branch(taken, block)?;
            if self.flow != Flow::Normal {
                self.pc = entry;
                return Ok(());
            }
        }

        if !open.is_false() && self.logic.is_sat(open) {
            if let Some(block) = &stmt.elseblock {
                self.run_branch(open, block)?;
            }
        }

        self.pc = entry;
        Ok(())
    }

    /// Execute a block under `pc`, absorbing an `error()` that only fires on
    /// some configurations.
    fn run_branch(&mut self, pc: Pc, block: &Block) -> eyre::Result<()> {
        let saved = mem::replace(&mut self.pc, pc);
        let r = self.block(block);
        self.pc = saved;
        r?;
        if self.flow == Flow::Abort {
            // The configurations that reached `error()` no longer exist; the
            // ones that did not carry on normally.
            self.flow = Flow::Normal;
        }
        Ok(())
    }

    fn exec_foreach(&mut self, stmt: &ForeachStmt) -> eyre::Result<()> {
        let iter = self.expr(&stmt.iter)?;
        let entry = self.pc;

        // Each variant of the iterable is a different sequence; run the loop
        // once per variant, under that variant's configurations.
        for variant in iter.variants().to_vec() {
            let base = self.logic.and(entry, variant.cond);
            if base.is_false() || !self.logic.is_sat(base) {
                continue;
            }
            let entries = self.loop_entries(&variant.value, &stmt.names, base)?;
            let last = entries.len().saturating_sub(1);

            for (i, (cond, bindings)) in entries.into_iter().enumerate() {
                let g = self.logic.and(base, cond);
                if g.is_false() || !self.logic.is_sat(g) {
                    continue;
                }
                self.with_pc(g, |this| {
                    for (name, value) in bindings {
                        this.assign(&name, Variational::from(Variant::new(g, value)));
                    }
                });

                let saved = mem::replace(&mut self.pc, g);
                let r = self.block(&stmt.body);
                self.pc = saved;
                r?;

                match self.flow {
                    Flow::Abort => {
                        self.flow = Flow::Normal;
                    }
                    Flow::Break => {
                        self.flow = Flow::Normal;
                        if cond.is_true() {
                            break;
                        }
                        // A `break` that only happens in some configurations
                        // would make every later iteration conditional on it,
                        // which no static build description can express.
                        bail!(
                            "`break` inside a loop over a configuration-dependent element \
                             has no static translation"
                        );
                    }
                    Flow::Continue => {
                        self.flow = Flow::Normal;
                        if !cond.is_true() && i != last {
                            bail!(
                                "`continue` inside a loop over a configuration-dependent \
                                 element has no static translation"
                            );
                        }
                    }
                    Flow::Normal => {}
                }
            }
        }

        self.pc = entry;
        Ok(())
    }

    /// The bindings each iteration makes, with the condition that iteration
    /// happens at all.
    fn loop_entries(
        &mut self,
        iterable: &Value,
        names: &[String],
        base: Pc,
    ) -> eyre::Result<Vec<(Pc, Vec<(String, Value)>)>> {
        match iterable {
            Value::List(items) => {
                if names.len() != 1 {
                    bail!("iterating a list takes exactly one loop variable");
                }
                Ok(items
                    .iter()
                    .map(|item| {
                        (item.cond, vec![(names[0].clone(), item.value.clone())])
                    })
                    .collect())
            }
            Value::Dict(items) => {
                if names.len() != 2 {
                    bail!("iterating a dict takes two loop variables");
                }
                Ok(items
                    .iter()
                    .map(|item| {
                        (item.cond, vec![
                            (names[0].clone(), Value::Str(item.value.0.clone())),
                            (names[1].clone(), item.value.1.clone()),
                        ])
                    })
                    .collect())
            }
            other => {
                let _ = base;
                bail!("cannot iterate over a {}", other.type_name())
            }
        }
    }

    // -- the store --------------------------------------------------------

    /// Write `value` under the current path condition, keeping whatever the
    /// variable held elsewhere.
    pub(crate) fn assign(&mut self, name: &str, value: Variational<Value>) {
        let pc = self.pc;
        let mut out = value.restrict(&mut self.logic, pc);

        if let Some(old) = self.vars.get(name).cloned() {
            let elsewhere = self.logic.not(pc);
            out.extend(old.restrict(&mut self.logic, elsewhere));
        }

        out.normalize(&mut self.logic);
        trace!(name, variants = out.len(), "assign");
        self.vars.insert(name.to_owned(), out);
    }

    /// `name += value`.
    ///
    /// A list grows in place rather than being rewritten: the appended elements
    /// carry the condition they were appended under, so the configurations that
    /// skipped the append still read the list they had. Rewriting the variable
    /// instead would fork it once per append, and a handful of independent
    /// `if`s appending to one list would multiply out into a variant per
    /// combination.
    pub(crate) fn plus_assign(
        &mut self,
        name: &str,
        rhs: &Variational<Value>,
    ) -> eyre::Result<()> {
        let old = self
            .vars
            .get(name)
            .cloned()
            .ok_or_else(|| eyre::eyre!("`+=` on undefined variable `{name}`"))?;

        if !old.is_empty() && old.variants().iter().all(|v| v.value.is_list()) {
            let pc = self.pc;
            let mut out = Variational::empty();
            for variant in old.variants() {
                let Value::List(items) = &variant.value else {
                    unreachable!("every variant was checked to be a list")
                };
                let base = self.logic.and(variant.cond, pc);
                let mut items = items.to_vec();
                if !base.is_false() {
                    items.extend(self.elements_under(rhs, base));
                }
                out.push(Variant::new(variant.cond, Value::list(items)));
            }
            self.vars.insert(name.to_owned(), out);
            return Ok(());
        }

        let old = old.restrict(&mut self.logic, self.pc);
        let sum = self.add(&old, rhs)?;
        self.assign(name, sum);
        Ok(())
    }

    /// The value of `name` in the configurations currently being executed.
    pub(crate) fn lookup(&mut self, name: &str) -> eyre::Result<Option<Variational<Value>>> {
        let Some(v) = self.vars.get(name).cloned() else {
            return Ok(None);
        };
        let pc = self.pc;
        let mut v = v.restrict(&mut self.logic, pc);
        // A variable written in both arms of an `if` keeps a variant per arm;
        // narrowing to the current path leaves contradictory ones behind, and
        // only the solver can tell that they are contradictory.
        v.normalize(&mut self.logic);
        if v.is_empty() {
            return Ok(None);
        }
        Ok(Some(v))
    }

    pub(crate) fn with_pc<R>(&mut self, pc: Pc, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = mem::replace(&mut self.pc, pc);
        let r = f(self);
        self.pc = saved;
        r
    }

    // -- expressions ------------------------------------------------------

    pub(crate) fn expr(&mut self, expr: &Expr) -> eyre::Result<Variational<Value>> {
        match expr {
            Expr::Id(name) => self
                .lookup(name)?
                .ok_or_else(|| eyre::eyre!("undefined variable `{name}`")),
            Expr::String(v) => Ok(self.pure(Value::str(v))),
            Expr::FormatString(v) => self.format_string(v),
            Expr::Int(v) => Ok(self.pure(Value::Int(*v))),
            Expr::Bool(v) => Ok(self.pure(Value::Bool(*v))),
            Expr::List(items) => {
                let mut out = Vec::new();
                for item in items {
                    let v = self.expr(item)?;
                    out.extend(v.into_variants());
                }
                Ok(self.pure(Value::list(out)))
            }
            Expr::Dict(dict) => {
                let mut out = Vec::new();
                for key in &dict.order {
                    let value = dict.args.get(key).expect("dict order names its own keys");
                    let value = self.expr(value)?;
                    let key: Rc<str> = Rc::from(key.as_str());
                    for v in value.into_variants() {
                        out.push(Variant::new(v.cond, (key.clone(), v.value)));
                    }
                }
                Ok(self.pure(Value::dict(out)))
            }
            Expr::Call(call) => {
                let args = self.eval_args(&call.args)?;
                self.call(&call.name, &args, call.loc)
                    .wrap_err_with(|| format!("in `{}()` at {}", call.name, self.here(call.loc)))
            }
            Expr::Method(method) => {
                let obj = self.expr(&method.obj)?;
                let args = self.eval_args(&method.args)?;
                self.method(&obj, &method.name, &args, method.loc)
                    .wrap_err_with(|| {
                        format!("in `.{}()` at {}", method.name, self.here(method.loc))
                    })
            }
            Expr::Index(index) => {
                let obj = self.expr(&index.obj)?;
                let idx = self.expr(&index.index)?;
                self.index(&obj, &idx)
            }
            Expr::UnOp(UnOp { kind, val }) => {
                let v = self.expr(val)?;
                match kind {
                    UnOpKind::Not => {
                        let t = self.truth(&v)?;
                        let n = self.logic.not(t);
                        Ok(self.bool_value(n))
                    }
                    UnOpKind::Neg => self.map1(&v, |v| match v {
                        Value::Int(i) => Ok(Value::Int(-i)),
                        other => bail!("cannot negate a {}", other.type_name()),
                    }),
                }
            }
            Expr::BinOp(op) => self.binop(op),
            Expr::Ternary(Ternary {
                condition,
                trueblock,
                falseblock,
            }) => {
                let c = self.expr(condition)?;
                let c = self.truth(&c)?;
                let nc = self.logic.not(c);
                let then_pc = self.logic.and(self.pc, c);
                let else_pc = self.logic.and(self.pc, nc);

                let mut out = Variational::empty();
                if !then_pc.is_false() {
                    let v = self.with_pc(then_pc, |this| this.expr(trueblock))?;
                    out.extend(v.restrict(&mut self.logic, then_pc));
                }
                if !else_pc.is_false() {
                    let v = self.with_pc(else_pc, |this| this.expr(falseblock))?;
                    out.extend(v.restrict(&mut self.logic, else_pc));
                }
                out.normalize(&mut self.logic);
                Ok(out)
            }
        }
    }

    /// A value that holds everywhere the current path does.
    pub(crate) fn pure(&self, value: Value) -> Variational<Value> {
        Variant::new(self.pc, value).into()
    }

    /// Turn a condition into a boolean value.
    pub(crate) fn bool_value(&mut self, cond: Pc) -> Variational<Value> {
        let pc = self.pc;
        let yes = self.logic.and(pc, cond);
        let no = {
            let n = self.logic.not(cond);
            self.logic.and(pc, n)
        };
        let mut out = Variational::empty();
        out.push(Variant::new(yes, Value::Bool(true)));
        out.push(Variant::new(no, Value::Bool(false)));
        out
    }

    /// Like [`Self::bool_value`], but `"1"`/`"0"`: how a `pkg-config`
    /// availability flag reads, e.g. `graphene_has_sse2`.
    pub(crate) fn flag_value(&mut self, cond: Pc) -> Variational<Value> {
        let pc = self.pc;
        let yes = self.logic.and(pc, cond);
        let no = {
            let n = self.logic.not(cond);
            self.logic.and(pc, n)
        };
        let mut out = Variational::empty();
        out.push(Variant::new(yes, Value::str("1")));
        out.push(Variant::new(no, Value::str("0")));
        out
    }

    /// The configurations in which a value is true.
    pub(crate) fn truth(&mut self, value: &Variational<Value>) -> eyre::Result<Pc> {
        let mut out = Pc::FALSE;
        for variant in value.variants() {
            match &variant.value {
                Value::Bool(true) => out = self.logic.or(out, variant.cond),
                Value::Bool(false) => {}
                // Falsy on its own, the same as `.found()` on it.
                Value::Obj(Obj::Disabler) => {}
                Value::Unset => bail!("a condition read a value that was never set"),
                other => bail!("expected a bool in a condition, found a {}", other.type_name()),
            }
        }
        Ok(out)
    }

    /// Apply a total function to every variant.
    pub(crate) fn map1(
        &mut self,
        v: &Variational<Value>,
        mut f: impl FnMut(&Value) -> eyre::Result<Value>,
    ) -> eyre::Result<Variational<Value>> {
        let mut out = Variational::empty();
        for variant in v.variants() {
            out.push(Variant::new(variant.cond, f(&variant.value)?));
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }

    fn here(&self, loc: Loc) -> String {
        format!("{}/meson.build:{loc}", self.cur_dir().display())
    }

    // -- configuration variables -----------------------------------------

    /// The variable standing for a build option, creating it on first use.
    ///
    /// Returns `None` when the option has no finite domain (a free-form string,
    /// say) and so cannot be branched over.
    pub(crate) fn option_var(&mut self, name: &str) -> eyre::Result<Option<VarId>> {
        if let Some(id) = self.option_vars.get(name) {
            return Ok(Some(*id));
        }
        let Some(decl) = self.option_decl(name) else {
            return Ok(None);
        };
        let Some((choices, default)) = decl.kind.domain() else {
            return Ok(None);
        };
        // `project(default_options: ...)` moves the default, which matters
        // because the backend reports it as the out-of-the-box configuration.
        let default = self
            .default_options
            .get(name)
            .and_then(|v| choices.iter().position(|c| &**c == &**v))
            .unwrap_or(default);

        // An option the project declares is its own; anything else came from
        // meson, and means the same thing in every project.
        let kind = if self.options.contains_key(name) {
            VarKind::Option
        } else {
            VarKind::BuiltinOption
        };

        let id = self.logic.declare(Var {
            key: format!("option:{name}"),
            description: decl.description.clone(),
            kind,
            choices,
            default,
        });
        self.option_vars.insert(name.to_owned(), id);
        Ok(Some(id))
    }

    /// The declaration of `name`, whether it came from the project's option
    /// file or is one of meson's own.
    pub(crate) fn option_decl(&self, name: &str) -> Option<ProjectOption> {
        if let Some(decl) = self.options.get(name) {
            return Some(decl.clone());
        }
        builtins::builtin_option(name)
    }

    /// A yes/no variable for something the importer cannot answer: whether a
    /// header exists, whether a library is installed, and so on.
    pub(crate) fn probe_var(&mut self, key: &str, description: String) -> VarId {
        if let Some(id) = self.probe_vars.get(key) {
            return *id;
        }

        let external = ["dep:", "lib:"]
            .iter()
            .any(|prefix| key.starts_with(prefix));

        // Something the build has to find outside itself defaults to absent:
        // there is no configure step to look for it, and a generated build that
        // links a library nobody supplied fails outright, while one that leaves
        // it out usually still works.
        //
        // A compiler capability, by contrast, defaults to present, because that
        // is what a working toolchain normally reports.
        let (kind, default) = if external {
            (VarKind::Dependency, 1)
        } else {
            (VarKind::Probe, 0)
        };

        let id = self.logic.declare(Var {
            key: key.to_owned(),
            description: Some(description),
            kind,
            choices: vec!["true".to_owned(), "false".to_owned()],
            default,
        });
        self.probe_vars.insert(key.to_owned(), id);
        id
    }

    /// The condition for a probe having succeeded.
    pub(crate) fn probe(&mut self, key: &str, description: String) -> Pc {
        let id = self.probe_var(key, description);
        self.logic.lit(id, 0)
    }

    // -- graph construction ----------------------------------------------

    /// The target standing for something the build does not produce itself.
    pub(crate) fn external(&mut self, key: &str, label: &str, kind: External) -> TargetId {
        if let Some(id) = self.externals.get(key) {
            return *id;
        }
        let dir = self.cur_dir().to_path_buf();
        let id = self
            .graph
            .add(label, &dir, Pc::TRUE, Kind::External(kind));
        self.externals.insert(key.to_owned(), id);
        id
    }

    pub(crate) fn dep_obj(&mut self, dep: Dep) -> Value {
        Value::Obj(Obj::Dep(Rc::new(dep)))
    }

    pub(crate) fn program_obj(&mut self, program: Program) -> Value {
        Value::Obj(Obj::Program(Rc::new(program)))
    }

    pub(crate) fn config_data(&mut self) -> Value {
        Value::Obj(Obj::ConfigData(Rc::new(RefCell::new(ConfigData::default()))))
    }

    // -- coercions --------------------------------------------------------

    /// Flatten nested lists into conditional elements, the way meson flattens
    /// list arguments.
    pub(crate) fn flatten(&mut self, v: &Variational<Value>, pc: Pc, out: &mut Vec<Variant<Value>>) {
        for variant in v.variants() {
            let cond = self.logic.and(pc, variant.cond);
            if cond.is_false() {
                continue;
            }
            match &variant.value {
                Value::List(items) => {
                    let inner: Variational<Value> = items.iter().cloned().collect();
                    self.flatten(&inner, cond, out);
                }
                Value::Unset => {}
                other => out.push(Variant::new(cond, other.clone())),
            }
        }
    }

    /// Expand only the outermost list layer.
    ///
    /// This is what `+` does: meson concatenates lists without flattening
    /// them, so a list of lists stays a list of lists. Only *arguments* get
    /// flattened all the way down, which is what [`Self::flat`] is for.
    pub(crate) fn elements(&mut self, v: &Variational<Value>) -> Vec<Variant<Value>> {
        let pc = self.pc;
        self.elements_under(v, pc)
    }

    /// [`Self::elements`], but under an explicit condition.
    pub(crate) fn elements_under(
        &mut self,
        v: &Variational<Value>,
        pc: Pc,
    ) -> Vec<Variant<Value>> {
        let mut out = Vec::new();
        for variant in v.variants() {
            let cond = self.logic.and(pc, variant.cond);
            if cond.is_false() {
                continue;
            }
            match &variant.value {
                Value::List(items) => {
                    for item in items.iter() {
                        let c = self.logic.and(cond, item.cond);
                        if c.is_false() {
                            continue;
                        }
                        out.push(Variant::new(c, item.value.clone()));
                    }
                }
                Value::Unset => {}
                other => out.push(Variant::new(cond, other.clone())),
            }
        }
        out
    }

    pub(crate) fn flat(&mut self, v: &Variational<Value>) -> Vec<Variant<Value>> {
        let pc = self.pc;
        let mut out = Vec::new();
        self.flatten(v, pc, &mut out);
        out
    }

    /// A list argument read as strings.
    pub(crate) fn strings(&mut self, v: &Variational<Value>) -> eyre::Result<Variational<Rc<str>>> {
        let mut out = Variational::empty();
        for variant in self.flat(v) {
            let s = string_arg(&variant.value)
                .ok_or_else(|| eyre::eyre!("expected a string, found a {}", variant.value.type_name()))?;
            out.push(Variant::new(variant.cond, s));
        }
        Ok(out)
    }

    /// An argument that must not vary and must be a single string, e.g. a
    /// target's name.
    pub(crate) fn one_string(&mut self, v: &Variational<Value>) -> eyre::Result<Rc<str>> {
        let flat = self.flat(v);
        match flat.as_slice() {
            [only] => string_arg(&only.value)
                .ok_or_else(|| eyre::eyre!("expected a string, found a {}", only.value.type_name())),
            [] => bail!("expected a string, found nothing"),
            _ => bail!(
                "expected a single string, but the value differs between configurations \
                 ({} variants)",
                flat.len()
            ),
        }
    }

    /// An argument that must be a single integer.
    pub(crate) fn one_int(&mut self, v: &Variational<Value>) -> eyre::Result<i64> {
        let flat = self.flat(v);
        match flat.as_slice() {
            [only] => only
                .value
                .as_int()
                .ok_or_else(|| eyre::eyre!("expected an integer, found a {}", only.value.type_name())),
            [] => bail!("expected an integer, found nothing"),
            _ => bail!("expected a single integer, but the value differs between configurations"),
        }
    }

    pub(crate) fn opt_string(
        &mut self,
        args: &CallArgs,
        name: &str,
    ) -> eyre::Result<Option<Rc<str>>> {
        match args.get(name) {
            Some(v) => self.one_string(v).map(Some),
            None => Ok(None),
        }
    }

    /// Read a list argument as build inputs.
    pub(crate) fn sources(&mut self, v: &Variational<Value>) -> eyre::Result<Variational<Source>> {
        let mut out = Variational::empty();
        for variant in self.flat(v) {
            let src = match &variant.value {
                // A bare string in a source list is relative to the directory
                // that mentioned it.
                Value::Str(s) => Source::File(PathBuf::from(self.resolve(s))),
                Value::Obj(Obj::File(path)) => Source::File(PathBuf::from(&**path)),
                Value::Obj(Obj::Target(id)) | Value::Obj(Obj::Output(id, _)) => {
                    Source::Generated(*id)
                }
                other => bail!("cannot use a {} as a source", other.type_name()),
            };
            out.push(Variant::new(variant.cond, src));
        }
        Ok(out)
    }

    /// Read a `dependencies:`-style argument as graph edges.
    pub(crate) fn deps(&mut self, v: &Variational<Value>) -> eyre::Result<Variational<TargetId>> {
        let mut out = Variational::empty();
        for variant in self.flat(v) {
            match &variant.value {
                Value::Obj(Obj::Dep(dep)) => {
                    // A dependency that was not found contributes nothing, so
                    // fold its `found` condition into the edge.
                    let cond = self.logic.and(variant.cond, dep.found);
                    out.push(Variant::new(cond, dep.target));
                }
                Value::Obj(Obj::Target(id)) => out.push(Variant::new(variant.cond, *id)),
                Value::Obj(Obj::Program(p)) => {
                    let cond = self.logic.and(variant.cond, p.found);
                    out.push(Variant::new(cond, p.target));
                }
                Value::Unset => {}
                other => bail!("cannot use a {} as a dependency", other.type_name()),
            }
        }
        Ok(out)
    }

    /// Read an `include_directories:` argument.
    pub(crate) fn include_dirs(
        &mut self,
        v: &Variational<Value>,
    ) -> eyre::Result<Variational<PathBuf>> {
        let mut out = Variational::empty();
        for variant in self.flat(v) {
            match &variant.value {
                Value::Obj(Obj::IncludeDirs(dirs)) => {
                    for dir in dirs.iter() {
                        out.push(Variant::new(variant.cond, PathBuf::from(dir)));
                    }
                }
                Value::Str(s) => {
                    out.push(Variant::new(variant.cond, PathBuf::from(self.resolve(s))))
                }
                other => bail!("cannot use a {} as an include directory", other.type_name()),
            }
        }
        Ok(out)
    }

    /// Give up on the configurations currently being executed: they hit an
    /// `error()` and do not configure.
    pub(crate) fn abort(&mut self) {
        self.flow = Flow::Abort;
    }

    pub(crate) fn warn_unsupported(&self, what: &str, loc: Loc) {
        warn!(at = %self.here(loc), "{what} is not modelled; ignoring");
    }
}

/// A value read the way meson reads a string argument: a plain string, or a
/// source-tree path standing in for one, as `find_program()` already has to
/// accept from `project_source_root() / 'scripts' / 'x.py'`.
fn string_arg(v: &Value) -> Option<Rc<str>> {
    match v {
        Value::Str(s) => Some(s.clone()),
        Value::Obj(Obj::File(p)) => Some(p.clone()),
        _ => None,
    }
}

/// Collapse `.`/`..` without touching the filesystem: the paths are project
/// relative and may not exist on this machine.
pub(crate) fn normalize_path(path: &Path) -> String {
    let mut out: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in path.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            Normal(c) => out.push(c),
            RootDir | Prefix(_) => {}
        }
    }
    out.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
