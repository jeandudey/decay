use {
    decay_meson_logic::{
        Logic,
        Pc,
        Solver,
        VarId,
        Variant,
        Variational, //
    },
    std::collections::{
        BTreeMap,
        BTreeSet, //
    },
};

/// How deep a rendered expression sits, so nested `select()`s line up.
type Depth = usize;

/// How many lines naming every value may cost over collapsing to a `DEFAULT`
/// before it stops being worth it.
///
/// Zero would collapse anything that repeats at all, including a one-line arm
/// where naming the value is clearer than hiding it behind `DEFAULT`.
const WORTH_COLLAPSING: usize = 8;

fn lines(text: &str) -> usize {
    text.lines().count()
}

/// Produces the text for a value at a given nesting depth.
type Render<'r> = dyn Fn(Depth) -> String + 'r;

/// Turns presence conditions into buck2 `select()` expressions.
///
/// A condition is an arbitrary boolean formula, but a `select()` is a lookup
/// keyed on mutually exclusive configuration values. The two are bridged by
/// splitting the formula one variable at a time: at each level the keys are the
/// choices of a single variable — which buck2 already knows cannot overlap —
/// and each key maps to what is left of the formula once that choice is fixed.
/// The result is a decision diagram written in Starlark.
pub struct Selects {
    /// Build-file label for each variable choice.
    labels: BTreeMap<(VarId, u32), String>,
    /// Variables whose every value has a label, and whose constraint declares a
    /// default — so listing all of them covers every configuration.
    exhaustive: BTreeSet<VarId>,
    /// The label of a constraint value no platform sets, used to mark a target
    /// that must not exist in a given configuration.
    pub impossible: String,
}

impl Selects {
    pub fn new(
        labels: BTreeMap<(VarId, u32), String>,
        exhaustive: BTreeSet<VarId>,
        impossible: String,
    ) -> Self {
        Self {
            labels,
            exhaustive,
            impossible,
        }
    }

    /// Render a `select()` over one variable, or `None` when the variable does
    /// not distinguish anything here.
    ///
    /// A constraint this backend declares itself always has a value, so every
    /// choice can be listed and no `DEFAULT` is needed — which reads better and
    /// is stricter, since an unhandled configuration becomes an error instead
    /// of silently taking a fallback. But listing every choice writes an arm
    /// out once per value, and where several values share a large arm that is
    /// pure duplication. So both forms are built and the exhaustive one is kept
    /// unless collapsing to a `DEFAULT` saves more than a few lines.
    ///
    /// A constraint supplied from outside may have values nobody told us about,
    /// and only the `DEFAULT` form is sound for those.
    fn select_on<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        var: VarId,
        depth: Depth,
        mut render: impl FnMut(&mut Logic<S>, u32) -> String,
    ) -> Option<String> {
        let choices = logic.var(var).choices.len() as u32;
        let default = logic.var(var).default as u32;

        let rendered: Vec<String> = (0..choices).map(|c| render(logic, c)).collect();
        if rendered.iter().all(|arm| arm == &rendered[0]) {
            return None;
        }

        let collapsed = self.collapsed(var, default, &rendered, depth);
        let Some(listed) = self.listed(var, &rendered, depth) else {
            return collapsed;
        };
        let Some(collapsed) = collapsed else {
            return Some(listed);
        };

        if lines(&listed) > lines(&collapsed) + WORTH_COLLAPSING {
            return Some(collapsed);
        }
        Some(listed)
    }

    /// Every value named explicitly, with no `DEFAULT`.
    ///
    /// Only possible for a constraint this backend declared, where the values
    /// are known to be all of them.
    fn listed(&self, var: VarId, rendered: &[String], depth: Depth) -> Option<String> {
        if !self.exhaustive.contains(&var) {
            return None;
        }
        let mut keyed = Vec::new();
        for (choice, arm) in rendered.iter().enumerate() {
            let label = self.label(var, choice as u32)?;
            keyed.push((label, arm.clone()));
        }
        Some(Self::write(&keyed, None, depth))
    }

    /// The default value's arm as `DEFAULT`, with the values that agree with it
    /// left out.
    fn collapsed(
        &self,
        var: VarId,
        default: u32,
        rendered: &[String],
        depth: Depth,
    ) -> Option<String> {
        let fallback = rendered.get(default as usize)?.clone();
        let mut keyed = Vec::new();
        for (choice, arm) in rendered.iter().enumerate() {
            let choice = choice as u32;
            if choice == default || arm == &fallback {
                continue;
            }
            // A value with no label of its own can only be left to `DEFAULT`,
            // which is exactly what happens here.
            if let Some(label) = self.label(var, choice) {
                keyed.push((label, arm.clone()));
            }
        }
        if keyed.is_empty() {
            return None;
        }
        Some(Self::write(&keyed, Some(&fallback), depth))
    }

    fn write<T: AsRef<str>>(
        arms: &[(String, T)],
        fallback: Option<&T>,
        depth: Depth,
    ) -> String {
        let pad = indent(depth + 1);
        let mut out = String::from("select({\n");
        for (key, value) in arms {
            out.push_str(&format!("{pad}{key:?}: {},\n", value.as_ref()));
        }
        if let Some(fallback) = fallback {
            out.push_str(&format!("{pad}\"DEFAULT\": {},\n", fallback.as_ref()));
        }
        out.push_str(&format!("{}}})", indent(depth)));
        out
    }

    /// Render `yes` where `cond` holds and `no` where it does not.
    pub fn render<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        cond: Pc,
        yes: &Render<'_>,
        no: &Render<'_>,
        depth: Depth,
    ) -> String {
        if cond.is_true() {
            return yes(depth);
        }
        if cond.is_false() {
            return no(depth);
        }

        let support = logic.support(cond);
        let Some(&var) = support.first() else {
            return yes(depth);
        };

        let rendered = self.select_on(logic, var, depth, |logic, choice| {
            let rest = logic.restrict(cond, var, choice);
            self.render(logic, rest, yes, no, depth + 1)
        });

        // The variable turned out not to distinguish anything here.
        rendered.unwrap_or_else(|| {
            let rest = logic.restrict(cond, var, logic.var(var).default as u32);
            self.render(logic, rest, yes, no, depth)
        })
    }

    /// Render a list attribute whose entries may each be conditional.
    ///
    /// Entries that always hold are emitted as a plain list; each distinct
    /// condition contributes one `select()` that is concatenated onto it, which
    /// keeps every `select()` independent and free of overlapping keys.
    pub fn render_list<S: Solver, T>(
        &self,
        logic: &mut Logic<S>,
        items: &Variational<T>,
        context: Pc,
        depth: Depth,
        mut render: impl FnMut(&T) -> String,
    ) -> String {
        // Group by condition, preserving declaration order: link order is
        // observable, so it must not be reshuffled.
        let mut groups: Vec<(Pc, Vec<String>)> = Vec::new();
        for Variant { cond, value } in items {
            let cond = simplify(logic, *cond, context);
            if cond.is_false() {
                continue;
            }
            let text = render(value);
            match groups.iter_mut().find(|(c, _)| *c == cond) {
                Some((_, values)) => values.push(text),
                None => groups.push((cond, vec![text])),
            }
        }

        let mut parts: Vec<String> = Vec::new();
        for (cond, values) in &groups {
            let yes = |depth: Depth| list(values, depth);
            if cond.is_true() {
                parts.push(yes(depth));
            } else {
                parts.push(self.render(logic, *cond, &yes, &|_| "[]".to_owned(), depth));
            }
        }

        match parts.len() {
            0 => "[]".to_owned(),
            1 => parts.pop().unwrap(),
            _ => parts.join(" + "),
        }
    }

    /// Render a conditional mapping as a single `select()` of dicts.
    ///
    /// Unlike lists and strings, buck2 will not concatenate dict attributes, so
    /// the conditions cannot each contribute a piece. Instead the whole mapping
    /// is resolved per configuration: split on one variable at a time until
    /// every entry is either definitely in or definitely out, then write the
    /// dict that results.
    pub fn render_dict<S: Solver, T>(
        &self,
        logic: &mut Logic<S>,
        items: &Variational<T>,
        context: Pc,
        depth: Depth,
        mut render: impl FnMut(&T) -> (String, String),
    ) -> String {
        let mut entries: Vec<(Pc, String, String)> = Vec::new();
        for Variant { cond, value } in items {
            let cond = simplify(logic, *cond, context);
            if cond.is_false() {
                continue;
            }
            let (key, value) = render(value);
            // A header reached by two paths only needs listing once.
            if let Some(slot) = entries.iter_mut().find(|(_, k, _)| *k == key) {
                slot.0 = logic.or(slot.0, cond);
                continue;
            }
            entries.push((cond, key, value));
        }
        self.dict_at(logic, &entries, depth)
    }

    fn dict_at<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        entries: &[(Pc, String, String)],
        depth: Depth,
    ) -> String {
        let open = entries.iter().find_map(|(cond, _, _)| {
            (!cond.is_true())
                .then(|| logic.support(*cond).first().copied())
                .flatten()
        });

        let Some(var) = open else {
            let pad = indent(depth + 1);
            if entries.is_empty() {
                return "{}".to_owned();
            }
            let mut out = String::from("{\n");
            for (_, key, value) in entries {
                out.push_str(&format!("{pad}{key:?}: {value},\n"));
            }
            out.push_str(&format!("{}}}", indent(depth)));
            return out;
        };

        let narrow = |logic: &mut Logic<S>, choice: u32| -> Vec<(Pc, String, String)> {
            entries
                .iter()
                .map(|(cond, key, value)| {
                    (logic.restrict(*cond, var, choice), key.clone(), value.clone())
                })
                .filter(|(cond, _, _)| !cond.is_false())
                .collect()
        };

        let rendered = self.select_on(logic, var, depth, |logic, choice| {
            let rest = narrow(logic, choice);
            self.dict_at(logic, &rest, depth + 1)
        });

        rendered.unwrap_or_else(|| {
            let rest = narrow(logic, logic.var(var).default as u32);
            self.dict_at(logic, &rest, depth)
        })
    }

    /// Render conditional words as concatenated Starlark strings.
    ///
    /// A command line cannot be assembled with `join()` the way a list
    /// attribute can, because a `select()` is opaque to Starlark: it can only
    /// be concatenated. Each group of words therefore becomes one string, and
    /// conditional groups become a `select()` that contributes either their
    /// words or nothing. Every part carries its own leading separator so the
    /// pieces compose in any order.
    pub fn render_words<S: Solver, T>(
        &self,
        logic: &mut Logic<S>,
        items: &Variational<T>,
        context: Pc,
        depth: Depth,
        separator: &str,
        mut render: impl FnMut(&T) -> String,
    ) -> Vec<String> {
        let mut groups: Vec<(Pc, Vec<String>)> = Vec::new();
        for Variant { cond, value } in items {
            let cond = simplify(logic, *cond, context);
            if cond.is_false() {
                continue;
            }
            let text = render(value);
            match groups.iter_mut().find(|(c, _)| *c == cond) {
                Some((_, values)) => values.push(text),
                None => groups.push((cond, vec![text])),
            }
        }

        let mut parts = Vec::new();
        for (cond, values) in &groups {
            let mut text = String::new();
            for value in values {
                text.push_str(separator);
                text.push_str(value);
            }
            let yes = |_: Depth| format!("{text:?}");
            if cond.is_true() {
                parts.push(yes(depth));
            } else {
                parts.push(self.render(logic, *cond, &yes, &|_| "\"\"".to_owned(), depth));
            }
        }
        parts
    }

    /// Render a value that differs between configurations as a single
    /// `select()`.
    ///
    /// Unlike a list, a scalar has exactly one answer per configuration, so the
    /// variants are resolved together into one decision diagram rather than
    /// being concatenated.
    pub fn render_one<S: Solver, T>(
        &self,
        logic: &mut Logic<S>,
        values: &Variational<T>,
        context: Pc,
        fallback: &str,
        depth: Depth,
        mut render: impl FnMut(&T) -> String,
    ) -> String {
        let mut arms: Vec<(Pc, String)> = Vec::new();
        for Variant { cond, value } in values {
            let cond = simplify(logic, *cond, context);
            if cond.is_false() {
                continue;
            }
            arms.push((cond, render(value)));
        }
        self.choose(logic, &arms, fallback, depth)
    }

    /// Split `arms` on one variable at a time until each is unconditional.
    fn choose<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        arms: &[(Pc, String)],
        fallback: &str,
        depth: Depth,
    ) -> String {
        if let Some((_, value)) = arms.iter().find(|(cond, _)| cond.is_true()) {
            return value.clone();
        }
        let Some(var) = arms
            .iter()
            .find_map(|(cond, _)| logic.support(*cond).first().copied())
        else {
            return fallback.to_owned();
        };

        let narrow = |logic: &mut Logic<S>, choice: u32| -> Vec<(Pc, String)> {
            arms.iter()
                .map(|(cond, value)| (logic.restrict(*cond, var, choice), value.clone()))
                .filter(|(cond, _)| !cond.is_false())
                .collect()
        };

        let rendered = self.select_on(logic, var, depth, |logic, choice| {
            let rest = narrow(logic, choice);
            self.choose(logic, &rest, fallback, depth + 1)
        });

        rendered.unwrap_or_else(|| {
            let rest = narrow(logic, logic.var(var).default as u32);
            self.choose(logic, &rest, fallback, depth)
        })
    }

    fn label(&self, var: VarId, choice: u32) -> Option<String> {
        self.labels.get(&(var, choice)).cloned()
    }
}

pub fn indent(depth: Depth) -> String {
    "    ".repeat(depth)
}

/// A Starlark list, one entry per line.
pub fn list(values: &[String], depth: Depth) -> String {
    if values.is_empty() {
        return "[]".to_owned();
    }
    let pad = indent(depth + 1);
    let mut out = String::from("[\n");
    for value in values {
        out.push_str(&format!("{pad}{value},\n"));
    }
    out.push_str(&format!("{}]", indent(depth)));
    out
}

/// Drop from `cond` whatever `context` already guarantees.
///
/// A source file that is only present when GLX is enabled, inside a target that
/// only exists when GLX is enabled, is just present — emitting the condition
/// again would be noise the reader has to disprove.
pub fn simplify<S: Solver>(logic: &mut Logic<S>, cond: Pc, context: Pc) -> Pc {
    if logic.entails(context, cond) {
        return Pc::TRUE;
    }
    let both = logic.and(context, cond);
    if !logic.is_sat(both) {
        return Pc::FALSE;
    }
    cond
}
