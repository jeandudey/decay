//! A buck2 backend for the build graph.
//!
//! Every configuration the executor left open becomes a buck2 constraint, and
//! every conditional attribute becomes a `select()` over those constraints, so
//! the generated build keeps the choices the meson build had.

use {
    crate::select::Selects,
    decay_build_ir::{
        CmdArg,
        Define,
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
/// Returns the generated build file, which says which constraints are worth
/// declaring: one nothing selects on configures nothing.
pub fn emit<S: Solver>(
    graph: &Graph,
    logic: &mut Logic<S>,
    known: &Labels,
    shared: &Shared,
    out: &Path,
    package: &str,
) -> eyre::Result<String> {
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

    // The build file first: it decides which constraints there is any point in
    // declaring.
    let build = build_file(graph, logic, &selects, known, package);
    let used = Used::new(&build);

    let constraints_dir = out.join(CONSTRAINTS);
    fs::create_dir_all(&constraints_dir).wrap_err("Failed to create the constraints directory")?;
    fs::write(
        constraints_dir.join("BUCK"),
        constraints_file(graph, &local, &names, known, &used, package),
    )
    .wrap_err("Failed to write the constraints build file")?;

    fs::write(out.join("BUCK"), &build).wrap_err("Failed to write the build file")?;

    Ok(build)
}

/// Which constraints the generated build files actually select on.
///
/// An option meson declares but that changes nothing in the output — libepoxy's
/// `docs` once there is no doxygen to run — is not a knob, it is a promise that
/// setting it would do something. So the constraint is left out rather than
/// declared for nobody.
#[derive(Debug, Default)]
pub struct Used {
    files: Vec<String>,
}

impl Used {
    fn new(build: &str) -> Self {
        Self {
            files: vec![build.to_owned()],
        }
    }

    /// Everything generated so far, since a constraint meson shares between
    /// projects may be selected on by any one of them.
    pub fn everywhere(files: impl IntoIterator<Item = String>) -> Self {
        Self {
            files: files.into_iter().collect(),
        }
    }

    fn selects_on(&self, package: &str, name: &str) -> bool {
        let label = format!("//{package}:{name}[");
        self.files.iter().any(|file| file.contains(&label))
    }
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
    /// Binary targets for the tools the build runs, keyed the same way.
    pub programs: BTreeMap<String, String>,
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
    used: &Used,
    package: &str,
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
        // Nothing in the build file keys on it, so setting it would change
        // nothing.
        if !used.selects_on(&format!("{package}/{CONSTRAINTS}"), name) {
            continue;
        }
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
    let _ = write!(out, "\n{}\n", comment(&describe(var)));
    let _ = write!(
        out,
        "constraint(\n    name = {name:?},\n    values = [{values}],\n    \
         default = {default:?},\n    visibility = [\"PUBLIC\"],\n)\n"
    );
    out
}

/// A block of text as a Starlark comment: every line gets its own `#`, since
/// a description can span several (a compiler probe's source snippet, say),
/// and only the first line of an unprefixed one is actually a comment.
fn comment(text: &str) -> String {
    text.lines().map(|line| format!("# {line}")).collect::<Vec<_>>().join("\n")
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

    let mut rules: Vec<Rendered> = Vec::new();
    for target in &graph.targets {
        if target.cond.is_false() {
            debug!(name = %target.name, "target exists in no configuration; skipping");
            continue;
        }
        let rendered = render_target(graph, logic, selects, known, target);
        if matches!(&rendered, Rendered::Raw(text) if text.is_empty()) {
            continue;
        }
        rules.push(rendered);
    }

    out.push_str(&hoist(&mut rules));
    for rule in &rules {
        out.push('\n');
        rule.write(&mut out);
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

/// One rule of the build file, still in pieces so that values several rules
/// share can be named before anything is written out.
enum Rendered {
    /// Text that is already final.
    Raw(String),
    Rule {
        comment: Option<String>,
        rule: &'static str,
        attrs: Vec<(&'static str, String)>,
    },
}

impl Rendered {
    fn write(&self, out: &mut String) {
        match self {
            Rendered::Raw(text) => out.push_str(text),
            Rendered::Rule {
                comment,
                rule,
                attrs,
            } => {
                if let Some(comment) = comment {
                    let _ = writeln!(out, "# meson: {comment}");
                }
                let _ = writeln!(out, "{rule}(");
                for (key, value) in attrs {
                    let _ = writeln!(out, "    {key} = {value},");
                }
                let _ = writeln!(out, ")");
            }
        }
    }
}

fn render_target<S: Solver>(
    graph: &Graph,
    logic: &mut Logic<S>,
    selects: &Selects,
    known: &Labels,
    target: &Target,
) -> Rendered {
    let mut attrs: Vec<(&'static str, String)> = Vec::new();
    let cond = target.cond;
    let a = &target.attrs;

    let rule = match &target.kind {
        Kind::External(external) => {
            return Rendered::Raw(render_external(target, external, known));
        }
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
                command(graph, logic, selects, known, target),
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
            let roots = include_roots(&a.include_dirs, &target.package);

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
                        selects.render_dict(
                            logic,
                            &header_aliases(graph, &a.headers, &roots),
                            cond,
                            1,
                            |(key, s)| (key.clone(), source(graph, s)),
                        ),
                    ));
                }
            } else {
                private.extend(a.headers.iter().cloned());
            }

            if !private.is_empty() {
                attrs.push((
                    "headers",
                    selects.render_dict(
                        logic,
                        &header_aliases(graph, &private, &roots),
                        cond,
                        1,
                        |(key, s)| (key.clone(), source(graph, s)),
                    ),
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
            selects.render_compat(logic, cond, 1),
        ));
    }

    // An imported project exists to be depended on from elsewhere in the
    // repository, so everything it declares is visible.
    attrs.push(("visibility", "[\"PUBLIC\"]".to_owned()));

    let named = !target.label.is_empty() && target.label != target.name;
    Rendered::Rule {
        comment: named.then(|| target.label.clone()),
        rule,
        attrs,
    }
}

/// Give a name to every value that several rules share.
///
/// The same block of warning flags spelled out on twenty test binaries is
/// twenty copies a reader has to compare before believing they are the same
/// thing. Naming it once says so, and leaves each rule short enough to read.
fn hoist(rules: &mut [Rendered]) -> String {
    /// How many lines a name has to save to be worth the indirection. Below
    /// this, looking the name up costs the reader more than the repetition did.
    const WORTH_NAMING: usize = 8;

    let mut seen: Vec<(String, &'static str, usize)> = Vec::new();
    for rule in rules.iter() {
        let Rendered::Rule { attrs, .. } = rule else {
            continue;
        };
        for (key, value) in attrs {
            match seen.iter_mut().find(|(text, _, _)| text == value) {
                Some((_, _, count)) => *count += 1,
                None => seen.push((value.clone(), key, 1)),
            }
        }
    }

    let worth: Vec<(String, &'static str)> = seen
        .into_iter()
        .filter(|(value, _, count)| {
            let lines = value.lines().count();
            // A one-line value is already as clear as its name would be.
            lines > 1 && (count - 1) * lines >= WORTH_NAMING
        })
        .map(|(value, key, _)| (value, key))
        .collect();

    // Named after what the attribute is, and where one attribute contributes
    // more than one value, after the constraints that tell them apart.
    let mut names: Vec<(String, String)> = Vec::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for (value, key) in &worth {
        let base = base_name(key);
        let alone = worth.iter().filter(|(_, k)| base_name(k) == base).count() == 1;
        let stem = match alone {
            true => format!("_{base}"),
            false => {
                let by = keyed_on(value).join("_");
                match by.is_empty() {
                    true => format!("_{base}"),
                    false => format!("_{base}_{by}"),
                }
            }
        };
        let mut name = stem.clone();
        let mut n = 1;
        while taken.contains(&name) {
            n += 1;
            name = format!("{stem}{n}");
        }
        taken.insert(name.clone());
        names.push((value.clone(), name));
    }

    if names.is_empty() {
        return String::new();
    }

    for rule in rules.iter_mut() {
        let Rendered::Rule { attrs, .. } = rule else {
            continue;
        };
        for (_, value) in attrs.iter_mut() {
            if let Some((_, name)) = names.iter().find(|(text, _)| text == value) {
                *value = name.clone();
            }
        }
    }

    let mut out = String::from(
        "\n# Values shared by several of the rules below, named so that each rule\n\
         # says what it is rather than how it was configured.\n",
    );
    for (value, name) in &names {
        let _ = writeln!(out, "{name} = {}", dedent(value));
    }
    out
}

/// What to call a value hoisted out of an attribute.
fn base_name(key: &str) -> &str {
    match key {
        "target_compatible_with" => "needs",
        "compiler_flags" => "cflags",
        "linker_flags" => "ldflags",
        "exported_preprocessor_flags" | "preprocessor_flags" => "cppflags",
        "exported_headers" => "headers",
        "public_include_directories" | "include_directories" => "includes",
        "exported_deps" => "deps",
        other => other,
    }
}

/// The constraints a value keys on, in the order they appear.
///
/// Two values of the same attribute differ because they answer different
/// questions, so naming the questions is what tells them apart.
fn keyed_on(value: &str) -> Vec<String> {
    /// The marker every generated constraint label has before its name.
    const MARK: &str = "constraints:";

    let mut out: Vec<String> = Vec::new();
    for (offset, _) in value.match_indices(MARK) {
        let rest = &value[offset + MARK.len()..];
        let Some(end) = rest.find('[') else {
            continue;
        };
        let name = &rest[..end];
        // Every conditional target mentions it, so it distinguishes nothing.
        if name == "unsatisfiable" || out.iter().any(|seen| seen == name) {
            continue;
        }
        out.push(name.to_owned());
        if out.len() == 3 {
            break;
        }
    }
    out
}

/// Shift a rendered value from inside a rule out to the left margin.
fn dedent(text: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(n, line)| match n {
            0 => line,
            _ => line.strip_prefix("    ").unwrap_or(line),
        })
        .collect::<Vec<_>>()
        .join("\n")
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
///
/// Tried in the order `include_directories()` actually lists them, the same
/// order the compiler would see them as `-I` flags: a header reachable
/// through more than one root is spelled the way the first one names it, not
/// however the longest happens to. `own_dir` is where the compiled target
/// itself was declared: meson always puts a target's own directory (and its
/// build-mirror) on the quote-include path, with no `include_directories()`
/// needed, so a generated header sitting right beside the sources that use
/// it is still found — but only once every declared root has had a chance,
/// since an explicit root always outranks that implicit fallback.
fn include_roots(dirs: &Variational<PathBuf>, own_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in dirs {
        if !out.contains(&entry.value) {
            out.push(entry.value.clone());
        }
    }
    if !out.iter().any(|p| p == own_dir) {
        out.push(own_dir.to_path_buf());
    }
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

/// Every path an `#include` could name this header by.
///
/// A header can land on more than one declared root at once, the same way
/// meson adds every one of them as a `-I`, and different files in the same
/// project spell the include differently: glib's own `glib/glib.h` is
/// reached as `<glib.h>` from within `glib/` (root `glib`) and as
/// `<glib/gversionmacros.h>` from `gobject/`, `gio/`, ... (root `""`) —
/// true of a real, checked-in header exactly as much as a generated one. One
/// canonical key would satisfy only one of those, so a header is registered
/// under every spelling its target's roots produce.
fn include_paths(graph: &Graph, source: &Source, roots: &[PathBuf]) -> Vec<String> {
    let path = logical_path(graph, source);
    let mut out: Vec<String> = Vec::new();
    for root in roots {
        if let Ok(rest) = path.strip_prefix(root) {
            let key = rest.display().to_string();
            if !out.contains(&key) {
                out.push(key);
            }
        }
    }
    if out.is_empty() {
        out.push(path.display().to_string());
    }
    out
}

/// Expand each header into one dict entry per spelling [`include_paths`]
/// finds for it, all pointing at the same underlying source.
fn header_aliases(
    graph: &Graph,
    sources: &Variational<Source>,
    roots: &[PathBuf],
) -> Variational<(String, Source)> {
    let mut out = Variational::empty();
    for entry in sources {
        for key in include_paths(graph, &entry.value, roots) {
            out.push(Variant::new(entry.cond, (key, entry.value.clone())));
        }
    }
    out
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
    known: &Labels,
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
            // `substitute` quotes the literal parts itself, so a fragment
            // spanning several words — or several lines, the way a
            // generated enum's boilerplate does — reaches the command as
            // one argument, not word-split by the shell, while any
            // `@OUTPUT@`-style marker embedded in it (`--outputdir=@OUTDIR@`,
            // say) still expands.
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
                    // A tool is run, not linked or copied, so it is named as
                    // an executable rather than by its output path — and it
                    // never becomes a target of this build.
                    Kind::External(External::Program { name, .. }) => match known.programs.get(name)
                    {
                        Some(label) => format!("$(exe {label})"),
                        None => name.clone(),
                    },
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

/// A `CmdArg::Literal`'s text, safe to hand a shell as one argument.
///
/// The plain text is quoted, same as any other literal; a substitution
/// marker is not, since what it expands to — `$OUT`, a buck2 `$(location
/// ...)`, a whole list of them for `@INPUT@` — has to stay live for the
/// shell to act on, not become the literal characters quoting it would
/// leave behind. The two are stitched back together the way a shell allows
/// adjacent quoted and unquoted text to run together as one word.
fn substitute(text: &str, inputs: &str) -> String {
    let markers: [(&str, &str); 4] = [
        ("@INPUT@", inputs),
        ("@OUTPUT@", "$OUT"),
        ("@OUTDIR@", OUT_DIR),
        ("@BASENAME@", "${OUT##*/}"),
    ];

    let mut out = String::new();
    let mut rest = text;
    loop {
        let next = markers
            .iter()
            .filter_map(|(marker, replacement)| Some((rest.find(marker)?, marker, replacement)))
            .min_by_key(|(at, ..)| *at);
        let Some((at, marker, replacement)) = next else {
            break;
        };
        if at > 0 {
            out.push_str(&shell_quote(&rest[..at]));
        }
        out.push_str(replacement);
        rest = &rest[at + marker.len()..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push_str(&shell_quote(rest));
    }
    out
}

/// The directory holding a genrule's single output.
const OUT_DIR: &str = "${OUT%/*}";

/// `defines`, plus an explicit [`DefineValue::Undef`] for whatever
/// configuration under `cond` names no value for a name that some other
/// configuration does.
///
/// A `#mesondefine` line has to become something in every configuration, and
/// meson's own rule for a name nobody set is `/* #undef NAME */` — the same
/// as this closes an open define set with, so a project that only ever
/// `.set()`s a name inside an `if` still gets a valid header outside it.
fn complete_defines<S: Solver>(logic: &mut Logic<S>, defines: &Variational<Define>, cond: Pc) -> Variational<Define> {
    let mut covered: BTreeMap<String, Pc> = BTreeMap::new();
    for variant in defines.variants() {
        let entry = covered.entry(variant.value.name.clone()).or_insert(Pc::FALSE);
        *entry = logic.or(*entry, variant.cond);
    }

    let mut out = defines.clone();
    for (name, cov) in covered {
        let not_covered = logic.not(cov);
        let gap = logic.and(cond, not_covered);
        if gap.is_false() || !logic.is_sat(gap) {
            continue;
        }
        out.push(Variant::new(gap, Define {
            name,
            value: DefineValue::Undef,
        }));
    }
    out.normalize(logic);
    out
}

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
        // A template substitutes a define in whichever of meson's two
        // template syntaxes it actually uses: `@NAME@`, left untouched where
        // a name goes unmentioned, or a whole `#mesondefine NAME` line,
        // which meson rewrites to `#define`/`#undef` even where a name goes
        // unmentioned — an unset name is an unset `#define`. A template only
        // ever uses one of these for a given name, so this just tries both;
        // the one that names nothing in the template matches nothing and
        // does not change it.
        let mut edits = selects.render_words(logic, &target.attrs.defines, target.cond, 1, " ", |define| {
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

        let complete = complete_defines(logic, &target.attrs.defines, target.cond);
        edits.extend(selects.render_words(logic, &complete, target.cond, 1, " ", |define| {
            let line = match &define.value {
                DefineValue::Quoted(v) => format!("#define {} \"{v}\"", define.name),
                DefineValue::Raw(v) => format!("#define {} {v}", define.name),
                DefineValue::Number(v) => format!("#define {} {v}", define.name),
                DefineValue::Flag => format!("#define {}", define.name),
                DefineValue::Undef => format!("/* #undef {} */", define.name),
            }
            .replace('|', "\\|");
            shell_quote(&format!(
                "-es|^#mesondefine[[:space:]]\\+{}\\>.*|{line}|",
                define.name
            ))
        }));

        let input = match template {
            Source::File(path) => file_arg(graph, path),
            Source::Generated(id) => format!("$(location :{})", graph.target(*id).name),
        };

        // An empty, always-present `-e` comes first so the command stays
        // valid `sed` even where a configuration substitutes nothing: with no
        // `-e` at all, sed reads its first non-option argument as the script
        // instead of an input file, and the template just becomes a syntax
        // error.
        let mut parts = vec!["\"sed\"".to_owned(), "\" -e ''\"".to_owned()];
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
