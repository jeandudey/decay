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
    ///
    /// `render` returns `None` for a value the surrounding condition already
    /// rules out. Those values are not what the reader is choosing between, so
    /// if the rest agree there is nothing to select on at all.
    fn select_on<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        var: VarId,
        depth: Depth,
        mut render: impl FnMut(&mut Logic<S>, u32) -> Option<String>,
    ) -> Option<String> {
        let choices = logic.var(var).choices.len() as u32;
        let default = logic.var(var).default as u32;

        let rendered: Vec<Option<String>> = (0..choices).map(|c| render(logic, c)).collect();

        let mut reachable = rendered.iter().flatten();
        let first = reachable.next()?.clone();
        if reachable.all(|arm| *arm == first) {
            return None;
        }

        // An unreachable value still needs something written for it, since the
        // keys have to cover the constraint. Giving it the fallback's text is
        // what makes it disappear into `DEFAULT` rather than reading as a case
        // anyone has to consider.
        let filler = rendered[default as usize].clone().unwrap_or(first);
        let rendered: Vec<String> = rendered
            .into_iter()
            .map(|arm| arm.unwrap_or_else(|| filler.clone()))
            .collect();

        let collapsed = self.collapsed(var, default, &rendered, depth);
        let Some(listed) = self.listed(var, &rendered, depth) else {
            return collapsed;
        };
        let Some(collapsed) = collapsed else {
            return Some(listed);
        };

        // Naming a value is worth a line or two, but not worth writing the same
        // block out twice: where several values share an arm of any size, the
        // reader is better served by one `DEFAULT` than by hunting for the
        // difference between two copies.
        let repeats = |arm: &String| lines(arm) > 1 && rendered.iter().filter(|a| *a == arm).count() > 1;
        if rendered.iter().any(repeats) || lines(&listed) > lines(&collapsed) + WORTH_COLLAPSING {
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

    /// Render `yes` where `cond` holds and `no` where it does not, for a reader
    /// who already knows `context` holds.
    ///
    /// The context is what keeps the output flat. A target that only exists
    /// when three things are true does not need each of its attributes to ask
    /// about those three things again: within the target they are settled, and
    /// only what is still open gets a `select()`.
    pub fn render<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        cond: Pc,
        context: Pc,
        yes: &Render<'_>,
        no: &Render<'_>,
        depth: Depth,
    ) -> String {
        let cond = simplify(logic, cond, context);
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
            let known = logic.restrict(context, var, choice);
            if known.is_false() {
                return None;
            }
            let rest = logic.restrict(cond, var, choice);
            Some(self.render(logic, rest, known, yes, no, depth + 1))
        });

        // The variable turned out not to distinguish anything here.
        rendered.unwrap_or_else(|| {
            let choice = self.settled(logic, var, context);
            let known = logic.restrict(context, var, choice);
            let rest = logic.restrict(cond, var, choice);
            self.render(logic, rest, known, yes, no, depth)
        })
    }

    /// A value of `var` the context allows, preferring the one a build gets by
    /// default.
    ///
    /// Used to carry on down a variable that turned out not to matter: any
    /// value it can still take answers for all of them.
    fn settled<S: Solver>(&self, logic: &mut Logic<S>, var: VarId, context: Pc) -> u32 {
        let default = logic.var(var).default as u32;
        if !logic.restrict(context, var, default).is_false() {
            return default;
        }
        let choices = logic.var(var).choices.len() as u32;
        (0..choices)
            .find(|&choice| !logic.restrict(context, var, choice).is_false())
            .unwrap_or(default)
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
        let mut group = groups.iter().peekable();
        while let Some((cond, values)) = group.next() {
            let yes = |depth: Depth| list(values, depth);
            if cond.is_true() {
                parts.push(yes(depth));
                continue;
            }
            // A group and the one that answers the opposite question are one
            // choice, not two, and reading them side by side is what shows
            // that between them they cover every configuration.
            match group.next_if(|(next, _)| opposite(logic, *cond, *next, context)) {
                Some((_, otherwise)) => {
                    let no = |depth: Depth| list(otherwise, depth);
                    parts.push(self.render(logic, *cond, context, &yes, &no, depth));
                }
                None => {
                    parts.push(self.render(logic, *cond, context, &yes, &|_| "[]".to_owned(), depth))
                }
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
        self.dict_at(logic, &entries, context, depth)
    }

    fn dict_at<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        entries: &[(Pc, String, String)],
        context: Pc,
        depth: Depth,
    ) -> String {
        let mut entries: Vec<(Pc, String, String)> = entries
            .iter()
            .map(|(cond, key, value)| (simplify(logic, *cond, context), key.clone(), value.clone()))
            .filter(|(cond, _, _)| !cond.is_false())
            .collect();
        let entries = &mut entries;

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
            let known = logic.restrict(context, var, choice);
            if known.is_false() {
                return None;
            }
            let rest = narrow(logic, choice);
            Some(self.dict_at(logic, &rest, known, depth + 1))
        });

        rendered.unwrap_or_else(|| {
            let choice = self.settled(logic, var, context);
            let known = logic.restrict(context, var, choice);
            let rest = narrow(logic, choice);
            self.dict_at(logic, &rest, known, depth)
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

        let joined = |values: &Vec<String>| {
            let mut text = String::new();
            for value in values {
                text.push_str(separator);
                text.push_str(value);
            }
            text
        };

        let mut parts = Vec::new();
        let mut group = groups.iter().peekable();
        while let Some((cond, values)) = group.next() {
            let text = joined(values);
            let yes = |_: Depth| format!("{text:?}");
            if cond.is_true() {
                parts.push(yes(depth));
                continue;
            }
            // The two answers to one question belong in one `select()`. Only
            // neighbours are paired up, so nothing changes place: the order
            // these are written in is the order they reach the command line.
            match group.next_if(|(next, _)| opposite(logic, *cond, *next, context)) {
                Some((_, otherwise)) => {
                    let otherwise = joined(otherwise);
                    let no = |_: Depth| format!("{otherwise:?}");
                    parts.push(self.render(logic, *cond, context, &yes, &no, depth));
                }
                None => parts.push(self.render(
                    logic,
                    *cond,
                    context,
                    &yes,
                    &|_| "\"\"".to_owned(),
                    depth,
                )),
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
        self.choose(logic, &arms, context, fallback, depth)
    }

    /// Split `arms` on one variable at a time until each is unconditional.
    fn choose<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        arms: &[(Pc, String)],
        context: Pc,
        fallback: &str,
        depth: Depth,
    ) -> String {
        let arms: Vec<(Pc, String)> = arms
            .iter()
            .map(|(cond, value)| (simplify(logic, *cond, context), value.clone()))
            .filter(|(cond, _)| !cond.is_false())
            .collect();
        let arms = &arms;

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
            let known = logic.restrict(context, var, choice);
            if known.is_false() {
                return None;
            }
            let rest = narrow(logic, choice);
            Some(self.choose(logic, &rest, known, fallback, depth + 1))
        });

        rendered.unwrap_or_else(|| {
            let choice = self.settled(logic, var, context);
            let known = logic.restrict(context, var, choice);
            let rest = narrow(logic, choice);
            self.choose(logic, &rest, known, fallback, depth)
        })
    }

    /// The `target_compatible_with` of a target that only exists in some
    /// configurations.
    ///
    /// That attribute is itself a conjunction — every value listed has to hold
    /// — so whatever the condition demands outright is written as a plain list.
    /// Only what is left, a value ruled out or a choice between alternatives,
    /// needs a `select()`, and it is usually one level deep instead of four.
    pub fn render_compat<S: Solver>(
        &self,
        logic: &mut Logic<S>,
        cond: Pc,
        depth: Depth,
    ) -> String {
        let mut required = Vec::new();
        let mut rest = cond;
        while let Some((label, var, choice)) = self.forced(logic, rest) {
            required.push(label);
            rest = logic.restrict(rest, var, choice);
        }

        let mut parts = Vec::new();
        if !required.is_empty() {
            parts.push(list(&required, depth));
        }
        if !rest.is_true() {
            let impossible = format!("{:?}", self.impossible);
            parts.push(self.render(
                logic,
                rest,
                Pc::TRUE,
                &|_| "[]".to_owned(),
                &|depth| list(&[impossible.clone()], depth),
                depth,
            ));
        }

        match parts.len() {
            0 => "[]".to_owned(),
            1 => parts.pop().expect("just checked"),
            _ => parts.join(" + "),
        }
    }

    /// One constraint value `cond` cannot hold without, if there is one.
    fn forced<S: Solver>(&self, logic: &mut Logic<S>, cond: Pc) -> Option<(String, VarId, u32)> {
        for var in logic.support(cond) {
            let choices = logic.var(var).choices.len() as u32;
            for choice in 0..choices {
                // A value with no label cannot be asked for, only fallen back
                // to, so it is not something to require.
                let Some(label) = self.label(var, choice) else {
                    continue;
                };
                let lit = logic.lit(var, choice);
                if logic.entails(cond, lit) {
                    return Some((format!("{label:?}"), var, choice));
                }
            }
        }
        None
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

/// Whether two conditions are each other's answer: never both, never neither.
///
/// `#define X 1` and `#define X 0` are one question written twice, and the
/// reader can only see that they cover everything if they sit together.
fn opposite<S: Solver>(logic: &mut Logic<S>, a: Pc, b: Pc, context: Pc) -> bool {
    let both = logic.and(a, b);
    let both = logic.and(context, both);
    if logic.is_sat(both) {
        return false;
    }
    let na = logic.not(a);
    let nb = logic.not(b);
    let neither = logic.and(na, nb);
    let neither = logic.and(context, neither);
    !logic.is_sat(neither)
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
