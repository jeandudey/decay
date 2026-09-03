use {
    decay_meson_logic::{
        Var,
        VarKind, //
    },
    std::collections::{
        BTreeMap,
        BTreeSet, //
    },
};

/// A build-file name for every configuration variable.
///
/// Variable keys are qualified so the executor can keep an option and a
/// dependency of the same name apart, but that qualification is noise in a
/// build file: `glx[yes]` reads better than `option_glx__yes`. So each variable
/// asks for the plainest name it can have, and only falls back to a qualified
/// one when something else already took it.
/// A name for each variable, keyed the way the executor keys them.
///
/// Names only have to be unique within the package they are declared in, so
/// each package is named on its own: a project's `egl` option and the shared
/// "is libEGL there" constraint are different labels and can both keep the
/// plain name.
pub fn assign_by_key(vars: &[Var]) -> BTreeMap<String, String> {
    pick(vars)
        .into_iter()
        .enumerate()
        .map(|(index, name)| (vars[index].key.clone(), name))
        .collect()
}

/// A name for each variable, in the order the variables were given.
pub fn pick(vars: &[Var]) -> Vec<String> {
    // Options are the knobs a person actually reaches for, so they get first
    // claim on the short names; probes, which are numerous and rarely named by
    // hand, go last.
    let mut order: Vec<usize> = (0..vars.len()).collect();
    order.sort_by_key(|&i| match vars[i].kind {
        VarKind::Option => 0,
        VarKind::BuiltinOption => 1,
        VarKind::Machine => 2,
        VarKind::Dependency => 3,
        VarKind::Probe => 4,
    });

    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut out = vec![String::new(); vars.len()];

    for index in order {
        let name = candidates(&vars[index])
            .into_iter()
            .find(|name| !taken.contains(name))
            .unwrap_or_else(|| sanitize(&vars[index].key));
        taken.insert(name.clone());
        out[index] = name;
    }

    out
}

/// Names a variable would like, from plainest to most qualified.
fn candidates(var: &Var) -> Vec<String> {
    let parts: Vec<&str> = var.key.split(':').collect();
    let mut out = Vec::new();

    match parts.as_slice() {
        ["option", name] => {
            out.push(sanitize(name));
            out.push(format!("option_{}", sanitize(name)));
        }
        ["machine", machine, property] => {
            out.push(sanitize(property));
            out.push(format!("{}_{}", sanitize(machine), sanitize(property)));
        }
        ["compiler", lang] => {
            out.push("compiler".to_owned());
            out.push(format!("{}_compiler", sanitize(lang)));
        }
        ["probe", lang, check, what] => {
            out.push(format!("{}_{}", sanitize(check), sanitize(what)));
            out.push(format!(
                "{}_{}_{}",
                sanitize(lang),
                sanitize(check),
                sanitize(what)
            ));
        }
        ["dep", name] => {
            out.push(sanitize(name));
            out.push(format!("{}_dep", sanitize(name)));
        }
        ["lib", name] => {
            out.push(sanitize(name));
            out.push(format!("{}_lib", sanitize(name)));
        }
        ["prog", name] => {
            out.push(sanitize(name));
            out.push(format!("{}_prog", sanitize(name)));
        }
        _ => {}
    }

    out.push(sanitize(&var.key));
    out
}

/// The names a variable's choices take as constraint values.
///
/// Sanitising can map two choices onto the same name (`c++11` and `c--11`), and
/// a constraint cannot have the same value twice, so collisions are numbered
/// apart.
pub fn values(var: &Var) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for choice in &var.choices {
        let base = sanitize(choice);
        let mut name = base.clone();
        let mut n = 1;
        while out.contains(&name) {
            n += 1;
            name = format!("{base}{n}");
        }
        out.push(name);
    }
    out
}

/// Reduce a name to what buck2 accepts in a target name.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_sep = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_sep = false;
        } else if !last_was_sep && !out.is_empty() {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_end_matches('_');
    if trimmed.is_empty() {
        "unnamed".to_owned()
    } else {
        trimmed.to_owned()
    }
}
