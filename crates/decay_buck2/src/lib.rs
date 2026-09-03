//! A buck2 backend for the build graph.
//!
//! Every configuration the executor left open becomes a buck2 constraint, and
//! every conditional attribute becomes a `select()` over those constraints, so
//! the generated build keeps the choices the meson build had.

use {
    crate::select::{
        Selects,
        list, //
    },
    decay_build_ir::{
        CmdArg,
        DefineValue,
        External,
        Graph,
        Kind,
        Linkage,
        Origin,
        Source,
        Target,
        TargetId, //
    },
    decay_meson_logic::{
        ANY_OTHER,
        Logic,
        Pc,
        Solver,
        Variant,
        Variational,
        Var,
        VarId,
        VarKind, //
    },
    eyre::Context,
    std::{
        collections::{
            BTreeMap,
            BTreeSet, //
        },
        fmt::Write as _,
        fs,
        path::{
            Path,
            PathBuf, //
        },
    },
    tracing::debug,
};

mod names;
mod select;
mod shared;

pub use shared::Shared;

/// Where the constraints a project needs are declared, relative to the
/// project's own directory.
const CONSTRAINTS: &str = "constraints";

/// Prefix of the key of a variable standing for a constraint declared outside
/// the generated build; the rest of the key is the setting's label.
const CONSTRAINT: &str = "constraint:";

/// Generate build files for `graph` into `out`.
///
/// `package` is the build-file path of `out`, e.g. `third-party/meson/libepoxy`,
/// and is what generated labels are anchored to.
pub fn emit<S: Solver>(
    graph: &Graph,
    logic: &mut Logic<S>,
    known: &Labels,
    shared: &Shared,
    out: &Path,
    package: &str,
) -> eyre::Result<()> {
    fs::create_dir_all(out).wrap_err("Failed to create the output directory")?;

    // Only the project's own options are declared here; the rest are meson's,
    // and live in the package every import shares.
    let local: Vec<(VarId, Var)> = graph
        .options
        .iter()
        .enumerate()
        .filter(|(_, var)| !shared.owns(var))
        .map(|(index, var)| (VarId::from_index(index), var.clone()))
        .collect();

    let owned: Vec<Var> = local.iter().map(|(_, var)| var.clone()).collect();
    let names: BTreeMap<VarId, String> = local
        .iter()
        .map(|(id, _)| *id)
        .zip(names::pick(&owned))
        .collect();

    let (labels, exhaustive) = resolve_labels(graph, &names, known, shared, package);
    let selects = Selects::new(labels, exhaustive, shared.impossible());

    let constraints_dir = out.join(CONSTRAINTS);
    fs::create_dir_all(&constraints_dir).wrap_err("Failed to create the constraints directory")?;
    fs::write(
        constraints_dir.join("BUCK"),
        constraints_file(graph, &local, &names, known),
    )
    .wrap_err("Failed to write the constraints build file")?;

    let build = build_file(graph, logic, &selects, known, package);
    fs::write(out.join("BUCK"), build).wrap_err("Failed to write the build file")?;

    Ok(())
}

/// Build-file labels the importer was given for choices it should not invent
/// constraints for, keyed by the choice name.
#[derive(Debug, Default)]
pub struct Labels {
    pub systems: BTreeMap<String, String>,
    pub compilers: BTreeMap<String, String>,
    /// Targets that already provide a dependency the meson build looked up
    /// outside itself, keyed by the name meson used.
    pub dependencies: BTreeMap<String, String>,
}

impl Labels {
    fn lookup(&self, var: &Var, choice: &str) -> Option<String> {
        // A constraint the configuration named carries its own label: the
        // setting is the key and the choice is the value.
        if var.kind == VarKind::Constraint {
            let setting = var.key.strip_prefix(CONSTRAINT)?;
            if choice == ANY_OTHER {
                return None;
            }
            return Some(format!("{setting}[{choice}]"));
        }
        if var.key.starts_with("machine:") && var.key.ends_with(":system") {
            return self.systems.get(choice).cloned();
        }
        if var.key.starts_with("compiler:") {
            return self.compilers.get(choice).cloned();
        }
        None
    }

    /// Whether this variable is described by labels from outside, which may
    /// have values the importer never heard of.
    fn is_external(&self, var: &Var) -> bool {
        var.choices
            .iter()
            .any(|choice| self.lookup(var, choice).is_some())
    }
}

/// Pick the build-file label that selects each choice of each open variable.
///
/// Also reports which variables the generated constraints cover completely,
/// since only those can be selected on without a `DEFAULT`.
fn resolve_labels(
    graph: &Graph,
    names: &BTreeMap<VarId, String>,
    known: &Labels,
    shared: &Shared,
    package: &str,
) -> (BTreeMap<(VarId, u32), String>, BTreeSet<VarId>) {
    let mut labels = BTreeMap::new();
    let mut exhaustive = BTreeSet::new();

    for (index, var) in graph.options.iter().enumerate() {
        let id = VarId::from_index(index);
        let external = known.is_external(var);
        if !external {
            exhaustive.insert(id);
        }

        let values = names::values(var);
        for (choice, name) in var.choices.iter().enumerate() {
            let label = match known.lookup(var, name) {
                Some(label) => label,
                None if external => continue,
                // Meson's own constraints live in one place, shared by every
                // imported project.
                None => match shared.label(var, &values[choice]) {
                    Some(label) => label,
                    None => {
                        let setting = names.get(&id).expect("every local variable was named");
                        format!("//{package}/{CONSTRAINTS}:{setting}[{}]", values[choice])
                    }
                },
            };
            labels.insert((id, choice as u32), label);
        }
    }

    (labels, exhaustive)
}

/// The name of the constraint standing for "this configuration cannot exist".
/// The constraints the generated `select()`s key on.
///
/// They are generated rather than assumed so the output stands on its own: a
/// project's own options have no counterpart in the buck2 prelude. Each one
/// declares the value meson would have defaulted to, so a build that configures
/// nothing behaves the way `meson setup` with no arguments would.
fn constraints_file(
    graph: &Graph,
    local: &[(VarId, Var)],
    names: &BTreeMap<VarId, String>,
    known: &Labels,
) -> String {
    let mut out = format!(
        "# Generated by decay. Do not edit.\n\
         #\n\
         # The options {} declares, defaulting to what meson itself would have\n\
         # chosen. Set them on a platform to configure this project. Options\n\
         # meson provides to every project live alongside, one directory up.\n",
        graph.project.name,
    );

    for (id, var) in local {
        // A choice the importer was told how to select needs no constraint of
        // its own.
        if known.is_external(var) {
            continue;
        }
        let name = names.get(id).expect("every local variable was named");
        out.push_str(&render_constraint(var, name));
    }

    out
}

/// One `constraint()` rule, with the value meson would have defaulted to.
fn render_constraint(var: &Var, name: &str) -> String {
    let values = names::values(var);
    let default = values
        .get(var.default)
        .cloned()
        .unwrap_or_else(|| values[0].clone());
    let values = values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::new();
    let _ = write!(out, "\n# {}\n", describe(var));
    let _ = write!(
        out,
        "constraint(\n    name = {name:?},\n    values = [{values}],\n    \
         default = {default:?},\n    visibility = [\"PUBLIC\"],\n)\n"
    );
    out
}

fn describe(var: &Var) -> String {
    let what = match var.kind {
        VarKind::Option => "build option",
        VarKind::BuiltinOption => "meson build option",
        VarKind::Machine => "machine property",
        VarKind::Probe => "toolchain probe",
        VarKind::Dependency => "external dependency",
        VarKind::Constraint => "constraint",
    };
    match &var.description {
        Some(text) => format!("{what} `{}`: {text}", var.key),
        None => format!("{what} `{}`", var.key),
    }
}

// -- the build file -------------------------------------------------------

fn build_file<S: Solver>(
    graph: &Graph,
    logic: &mut Logic<S>,
    selects: &Selects,
    known: &Labels,
    package: &str,
) -> String {
    let mut out = format!(
        "# Generated by decay from the meson build of {}{}.\n\
         # Do not edit; re-run `decay buckify` instead.\n\
         #\n\
         # Configuration lives in //{package}/{CONSTRAINTS}.\n",
        graph.project.name,
        graph
            .project
            .version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default(),
    );

    if let Some(origin) = &graph.project.origin {
        out.push('\n');
        out.push_str(&render_fetch(graph, origin));
    }

    for target in &graph.targets {
        if target.cond.is_false() {
            debug!(name = %target.name, "target exists in no configuration; skipping");
            continue;
        }
        let rendered = render_target(graph, logic, selects, known, target);
        if rendered.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(&rendered);
    }

    if !graph.tests.is_empty() {
        out.push_str("\n# Tests declared by the meson build, as `name -> target`:\n");
        for test in &graph.tests {
            let _ = writeln!(
                out,
                "#   {} -> :{}",
                test.name,
                graph.target(test.target).name
            );
        }
    }

    out
}

/// The name of the target that fetches the project's sources.
fn repo_target(graph: &Graph) -> String {
    format!("{}.git", graph.project.name)
}

/// Fetch the sources instead of keeping a copy of them.
///
/// Every file the build refers to is listed as a sub-target, which is how a
/// rule elsewhere in the file names one: `:libepoxy.git[src/dispatch.c]`.
fn render_fetch(graph: &Graph, origin: &Origin) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "git_fetch(");
    let _ = writeln!(out, "    name = {:?},", repo_target(graph));
    let _ = writeln!(out, "    repo = {:?},", origin.repo);
    let _ = writeln!(out, "    rev = {:?},", origin.rev);
    let _ = writeln!(out, "    sub_targets = [");
    for path in referenced_files(graph) {
        let _ = writeln!(out, "        {path:?},");
    }
    let _ = writeln!(out, "    ],");
    let _ = writeln!(out, "    visibility = [\"PUBLIC\"],");
    let _ = writeln!(out, ")");
    out
}

/// Every file in the project that some rule refers to, sorted.
fn referenced_files(graph: &Graph) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();

    for target in &graph.targets {
        if target.cond.is_false() {
            continue;
        }

        let sources = target
            .attrs
            .srcs
            .iter()
            .chain(target.attrs.headers.iter())
            .map(|entry| &entry.value)
            .chain(target.attrs.template.iter());
        for source in sources {
            if let Source::File(path) = source {
                out.insert(path.display().to_string());
            }
        }

        for entry in &target.attrs.cmd {
            if let CmdArg::File(path) = &entry.value {
                out.insert(path.display().to_string());
            }
        }

        if let Kind::External(External::Program { path: Some(path), .. }) = &target.kind {
            out.insert(path.display().to_string());
        }
    }

    out.into_iter().collect()
}

fn render_target<S: Solver>(
    graph: &Graph,
    logic: &mut Logic<S>,
    selects: &Selects,
    known: &Labels,
    target: &Target,
) -> String {
    let mut attrs: Vec<(&str, String)> = Vec::new();
    let cond = target.cond;
    let a = &target.attrs;

    let rule = match &target.kind {
        Kind::External(external) => return render_external(target, external, known),
        Kind::Custom => "genrule",
        Kind::ConfigHeader => "genrule",
        Kind::Executable => "cxx_binary",
        _ => "cxx_library",
    };

    attrs.push(("name", format!("{:?}", target.name)));

    match &target.kind {
        Kind::Custom => {
            // Inputs reach the command through `$(location ...)`, which already
            // makes them dependencies of the rule.
            attrs.push(("out", format!("{:?}", a.outs.first().cloned().unwrap_or_default())));
            attrs.push((
                "cmd",
                command(graph, logic, selects, target),
            ));
        }
        Kind::ConfigHeader => {
            attrs.push(("out", format!("{:?}", a.outs.first().cloned().unwrap_or_default())));
            attrs.push(("cmd", config_header_cmd(graph, logic, selects, target)));
        }
        _ => {
            let is_interface = matches!(target.kind, Kind::Interface);
            let library = !matches!(target.kind, Kind::Executable);

            if !is_interface {
                attrs.push((
                    "srcs",
                    selects.render_list(logic, &a.srcs, cond, 1, |s| source(graph, s)),
                ));
            }

            // Include paths decide how a header is spelled in an `#include`,
            // so they are needed before the headers themselves. They are not
            // emitted as an attribute: the sources are fetched rather than
            // checked in, so there is no directory in this package to point at.
            // The header maps below carry the same information, keyed by the
            // path an `#include` actually uses.
            let roots = include_roots(&a.include_dirs);

            // A generated configuration header has no file in the source
            // tree, so every compiled target has to be told where it lands.
            // Meson gets this for free by mirroring the source layout into the
            // build directory and putting that on the include path.
            let mut private: Variational<Source> = graph
                .targets
                .iter()
                .filter(|t| is_config_header(t))
                // A header that only exists in some configurations would drag
                // every target that includes it into the same condition, so it
                // is only wired in where it is unconditionally present.
                .filter(|t| logic.entails(cond, t.cond))
                .map(|t| Variant::new(Pc::TRUE, Source::Generated(t.id)))
                .collect();

            // Headers are keyed by the path an `#include` uses, which is why
            // the namespace buck2 would otherwise prepend is cleared.
            if library {
                if !a.headers.is_empty() {
                    attrs.push((
                        "exported_headers",
                        selects.render_dict(logic, &a.headers, cond, 1, |s| {
                            (include_path(graph, s, &roots), source(graph, s))
                        }),
                    ));
                }
            } else {
                private.extend(a.headers.iter().cloned());
            }

            if !private.is_empty() {
                attrs.push((
                    "headers",
                    selects.render_dict(logic, &private, cond, 1, |s| {
                        (include_path(graph, s, &roots), source(graph, s))
                    }),
                ));
            }

            attrs.push(("header_namespace", "\"\"".to_owned()));

            if !a.compile_args.is_empty() {
                attrs.push((
                    "compiler_flags",
                    selects.render_list(logic, &a.compile_args, cond, 1, |s| format!("{s:?}")),
                ));
            }
            if !a.link_args.is_empty() {
                let key = if library {
                    "exported_linker_flags"
                } else {
                    "linker_flags"
                };
                attrs.push((
                    key,
                    selects.render_list(logic, &a.link_args, cond, 1, |s| format!("{s:?}")),
                ));
            }

            // `deps` and `link_with` differ in meson only by whether usage
            // requirements propagate, which buck2 decides per rule rather than
            // per edge.
            // `link_with:` before `dependencies:`, the order meson puts them
            // in. With static archives the order is not cosmetic: a library
            // listed before the archive that needs it resolves nothing.
            let mut deps: Variational<TargetId> = a
                .link_with
                .iter()
                .chain(a.deps.iter())
                // A program found on `PATH` has no target to depend on.
                .filter(|d| has_rule(graph.target(d.value)))
                .cloned()
                .collect();
            deps.normalize(logic);
            if !deps.is_empty() {
                // `declare_dependency()` exists to pass usage requirements on,
                // so its edges have to be exported; buck2 keeps plain `deps`
                // private to the target that declares them.
                let key = if is_interface { "exported_deps" } else { "deps" };
                attrs.push((
                    key,
                    selects.render_list(logic, &deps, cond, 1, |id| {
                        format!("\":{}\"", graph.target(*id).name)
                    }),
                ));
            }

            if let Kind::Library { linkage } = &target.kind {
                attrs.push((
                    "preferred_linkage",
                    selects.render_one(logic, linkage, cond, "\"any\"", 1, |l| {
                        format!("{:?}", linkage_name(*l))
                    }),
                ));
            } else if let Some(name) = static_linkage(&target.kind) {
                attrs.push(("preferred_linkage", format!("{name:?}")));
            }
        }
    }

    if !cond.is_true() {
        attrs.push((
            "target_compatible_with",
            selects.render(
                logic,
                cond,
                &|_| "[]".to_owned(),
                &|depth| list(&[format!("{:?}", selects.impossible)], depth),
                1,
            ),
        ));
    }

    // An imported project exists to be depended on from elsewhere in the
    // repository, so everything it declares is visible.
    attrs.push(("visibility", "[\"PUBLIC\"]".to_owned()));

    let mut out = String::new();
    if !target.label.is_empty() && target.label != target.name {
        let _ = writeln!(out, "# meson: {}", target.label);
    }
    let _ = writeln!(out, "{rule}(");
    for (key, value) in attrs {
        let _ = writeln!(out, "    {key} = {value},");
    }
    let _ = writeln!(out, ")");
    out
}

fn linkage_name(linkage: Linkage) -> &'static str {
    match linkage {
        Linkage::Static => "static",
        Linkage::Shared => "shared",
        // buck2 lets the consumer decide, which is the closest thing to
        // meson building both.
        Linkage::Both => "any",
    }
}

fn static_linkage(kind: &Kind) -> Option<&'static str> {
    match kind {
        Kind::StaticLibrary => Some("static"),
        Kind::SharedLibrary => Some("shared"),
        _ => None,
    }
}

/// Something the build does not produce itself.
///
/// The importer cannot know how a given repository wires up system libraries,
/// so these are emitted as stubs with the linker flag meson would have used and
/// a note saying what to replace them with.
fn render_external(target: &Target, external: &External, known: &Labels) -> String {
    let mut out = String::new();

    // Anything the importer was given a real target for is just an alias.
    if let Some(actual) = known.dependencies.get(&target.label) {
        let _ = writeln!(out, "# meson: {}", describe_external(external));
        let _ = writeln!(out, "alias(");
        let _ = writeln!(out, "    name = {:?},", target.name);
        let _ = writeln!(out, "    actual = {actual:?},");
        let _ = writeln!(out, "    visibility = [\"PUBLIC\"],");
        let _ = writeln!(out, ")");
        return out;
    }

    match external {
        // A program needs no rule either way: one that lives in the project
        // is reached through the fetch, and buck2 has no equivalent of looking
        // one up on `PATH`, so commands name that kind directly and rely on the
        // execution environment.
        External::Program { .. } => {}
        _ => {
            // `find_library` names the library itself, so `-l` is exactly
            // right. A pkg-config module name is not a library name and cannot
            // be turned into one without running pkg-config, which would bake
            // this machine's answer into the output — so the stub is left empty
            // for someone to point at a real target.
            let flags: Vec<String> = match external {
                External::SystemLibrary { name } => vec![format!("-l{name}")],
                External::Framework { modules } => modules
                    .iter()
                    .flat_map(|m| ["-framework".to_owned(), m.clone()])
                    .collect(),
                _ => Vec::new(),
            };

            let _ = writeln!(out, "# meson: {}", describe_external(external));
            if flags.is_empty() {
                let _ = writeln!(
                    out,
                    "# Empty stub: map it to a real target with `dependencies.{} = \"//some:target\"`",
                    target.label
                );
                let _ = writeln!(out, "# in decay.toml, or edit it here.");
            }
            let _ = writeln!(out, "cxx_library(");
            let _ = writeln!(out, "    name = {:?},", target.name);
            if !flags.is_empty() {
                let _ = writeln!(out, "    exported_linker_flags = [");
                for flag in flags {
                    let _ = writeln!(out, "        {flag:?},");
                }
                let _ = writeln!(out, "    ],");
            }
            let _ = writeln!(out, "    visibility = [\"PUBLIC\"],");
            let _ = writeln!(out, ")");
        }
    }
    out
}

fn describe_external(external: &External) -> String {
    match external {
        External::PkgConfig { module } => {
            format!("dependency({module:?}) — resolved by pkg-config")
        }
        External::SystemLibrary { name } => format!("find_library({name:?})"),
        External::Framework { modules } => {
            format!("dependency('appleframeworks', modules: {modules:?})")
        }
        External::Program { name, .. } => format!("find_program({name:?})"),
    }
}

/// The directories an `#include` is resolved against.
fn include_roots(dirs: &Variational<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in dirs {
        if !out.contains(&entry.value) {
            out.push(entry.value.clone());
        }
    }
    // Longest first, so the most specific root wins.
    out.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
    out
}

/// Where a header sits in the build tree, relative to the project root.
///
/// A generated header lands beside the `meson.build` that declared it, which is
/// how meson's mirrored build directory works.
fn logical_path(graph: &Graph, source: &Source) -> PathBuf {
    match source {
        Source::File(path) => path.clone(),
        Source::Generated(id) => {
            let target = graph.target(*id);
            let name = target
                .attrs
                .outs
                .first()
                .cloned()
                .unwrap_or_else(|| target.name.clone());
            target.package.join(name)
        }
    }
}

/// The path an `#include` would name this header by.
fn include_path(graph: &Graph, source: &Source, roots: &[PathBuf]) -> String {
    let path = logical_path(graph, source);
    for root in roots {
        if let Ok(rest) = path.strip_prefix(root) {
            return rest.display().to_string();
        }
    }
    path.display().to_string()
}

/// Whether a `configure_file()` produced something a compiler will include.
///
/// Meson uses the same function to fill in templates that have nothing to do
/// with C, and those must not be forced onto every compiled target.
fn is_config_header(target: &Target) -> bool {
    matches!(target.kind, Kind::ConfigHeader)
        && target
            .attrs
            .outs
            .first()
            .and_then(|name| Path::new(name).extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "h" | "hh" | "hpp" | "hxx"))
}

/// Whether a target turns into a rule a dependent can name.
fn has_rule(target: &Target) -> bool {
    !matches!(target.kind, Kind::External(External::Program { .. }))
}

fn source(graph: &Graph, source: &Source) -> String {
    match source {
        Source::File(path) => format!(
            "\":{}[{}]\"",
            repo_target(graph),
            path.display()
        ),
        Source::Generated(id) => format!("\":{}\"", graph.target(*id).name),
    }
}

/// A file as it is named on a command line.
fn file_arg(graph: &Graph, path: &Path) -> String {
    format!("$(location :{}[{}])", repo_target(graph), path.display())
}

/// The command line of a `custom_target`, with meson's placeholders rewritten
/// into the shell variables a genrule provides.
fn command<S: Solver>(
    graph: &Graph,
    logic: &mut Logic<S>,
    selects: &Selects,
    target: &Target,
) -> String {
    let inputs: Vec<String> = target
        .attrs
        .srcs
        .iter()
        .map(|s| match &s.value {
            Source::File(path) => file_arg(graph, path),
            Source::Generated(id) => format!("$(location :{})", graph.target(*id).name),
        })
        .collect();
    let inputs = inputs.join(" ");

    let words = selects.render_words(logic, &target.attrs.cmd, target.cond, 1, " ", |arg| {
        match arg {
            CmdArg::Literal(text) => substitute(text, &inputs),
            CmdArg::Target(id) => {
                let dep = graph.target(*id);
                match &dep.kind {
                    // A script that lives in the project is named through the
                    // fetch. A `.py` one is handed to an interpreter rather
                    // than executed, because a fetched file carries no promise
                    // about its execute bit.
                    Kind::External(External::Program { path: Some(path), .. }) => {
                        let location = file_arg(graph, path);
                        match path.extension().and_then(|e| e.to_str()) {
                            Some("py") => format!("python3 {location}"),
                            _ => location,
                        }
                    }
                    Kind::External(External::Program { name, .. }) => name.clone(),
                    _ => format!("$(location :{})", dep.name),
                }
            }
            CmdArg::File(path) => file_arg(graph, path),
            CmdArg::Inputs => inputs.clone(),
            CmdArg::Outputs => "$OUT".to_owned(),
            CmdArg::OutDir => OUT_DIR.to_owned(),
        }
    });

    join(&words)
}

/// Concatenate rendered command pieces into one Starlark expression.
fn join(parts: &[String]) -> String {
    if parts.is_empty() {
        return "\"\"".to_owned();
    }
    parts.join(" + ")
}

fn substitute(text: &str, inputs: &str) -> String {
    text.replace("@INPUT@", inputs)
        .replace("@OUTPUT@", "$OUT")
        .replace("@OUTDIR@", OUT_DIR)
        .replace("@BASENAME@", "${OUT##*/}")
}

/// The directory holding a genrule's single output.
const OUT_DIR: &str = "${OUT%/*}";

/// A generated configuration file.
///
/// With no template meson writes a C header from scratch; with one it
/// substitutes into it. Both are shell commands here, assembled so that each
/// entry can carry its own `select()`.
fn config_header_cmd<S: Solver>(
    graph: &Graph,
    logic: &mut Logic<S>,
    selects: &Selects,
    target: &Target,
) -> String {
    if let Some(template) = &target.attrs.template {
        let edits = selects.render_words(logic, &target.attrs.defines, target.cond, 1, " ", |define| {
            let value = match &define.value {
                DefineValue::Quoted(v) | DefineValue::Raw(v) => v.clone(),
                DefineValue::Number(v) => v.to_string(),
                DefineValue::Flag => "1".to_owned(),
                DefineValue::Undef => String::new(),
            };
            // `|` is the delimiter, so a value containing one would end the
            // expression early.
            let value = value.replace('|', "\\|");
            shell_quote(&format!("-es|@{}@|{value}|g", define.name))
        });

        let input = match template {
            Source::File(path) => file_arg(graph, path),
            Source::Generated(id) => format!("$(location :{})", graph.target(*id).name),
        };

        let mut parts = vec!["\"sed\"".to_owned()];
        parts.extend(edits);
        parts.push(format!("{:?}", format!(" {input} > $OUT")));
        return join(&parts);
    }

    let lines = selects.render_words(logic, &target.attrs.defines, target.cond, 1, " ", |define| {
        let text = match &define.value {
            DefineValue::Quoted(v) => format!("#define {} \"{}\"", define.name, v),
            DefineValue::Raw(v) => format!("#define {} {}", define.name, v),
            DefineValue::Number(v) => format!("#define {} {}", define.name, v),
            DefineValue::Flag => format!("#define {}", define.name),
            DefineValue::Undef => format!("/* #undef {} */", define.name),
        };
        shell_quote(&text)
    });

    let mut parts = vec!["\"printf '%s\\\\n'\"".to_owned()];
    parts.extend(lines);
    parts.push("\" > $OUT\"".to_owned());
    join(&parts)
}

/// Wrap a line so a shell passes it through untouched.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
