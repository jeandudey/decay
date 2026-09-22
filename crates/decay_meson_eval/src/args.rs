use {
    crate::{
        Interp,
        val::Value, //
    },
    decay_meson_ast::Args,
    decay_meson_logic::{
        Solver,
        Variant,
        Variational, //
    },
};

/// Evaluated call arguments.
///
/// Arguments stay variational rather than being split into one concrete call
/// per configuration: splitting turns a handful of independent options into a
/// combinatorial number of calls, and every builtin that matters can consume a
/// conditional list directly.
#[derive(Debug, Default)]
pub struct CallArgs {
    pub pos: Vec<Variational<Value>>,
    /// Keyword arguments in source order.
    pub kw: Vec<(String, Variational<Value>)>,
}

impl CallArgs {
    pub fn get(&self, name: &str) -> Option<&Variational<Value>> {
        self.kw.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    pub fn at(&self, index: usize) -> Option<&Variational<Value>> {
        self.pos.get(index)
    }

    /// Positional arguments after the first, which is how meson spells
    /// "sources" for most target functions.
    pub fn rest(&self) -> &[Variational<Value>] {
        self.pos.get(1..).unwrap_or(&[])
    }
}

impl<'a, S: Solver> Interp<'a, S> {
    pub(crate) fn eval_args(&mut self, args: &Args) -> eyre::Result<CallArgs> {
        let mut pos = Vec::with_capacity(args.pos.len());
        for arg in &args.pos {
            pos.push(self.expr(arg)?);
        }

        let mut kw = Vec::with_capacity(args.order.len());
        for name in &args.order {
            let expr = args
                .kw
                .get(name)
                .expect("keyword order names its own arguments");
            kw.push((name.clone(), self.expr(expr)?));
        }

        // `kwargs: some_dict` splats a dict as additional keyword arguments —
        // fontconfig's `library('fontconfig', ..., kwargs: lib_fontconfig_kwargs)`
        // is how its `dependencies:`/`include_directories:`/`link_with:` are
        // actually passed. An explicit keyword given alongside `kwargs:` wins
        // over the same key inside the dict, same as meson's own error case
        // but permissive instead of refusing the call.
        if let Some(pos) = kw.iter().position(|(name, _)| name == "kwargs") {
            let (_, kwargs_val) = kw.remove(pos);
            let explicit: Vec<String> = kw.iter().map(|(name, _)| name.clone()).collect();
            let mut splatted: Vec<(String, Variational<Value>)> = Vec::new();
            for outer in kwargs_val.variants() {
                let Value::Dict(entries) = &outer.value else {
                    continue;
                };
                for entry in entries.iter() {
                    let (key, value) = &entry.value;
                    let cond = self.logic.and(outer.cond, entry.cond);
                    if cond.is_false() || explicit.iter().any(|name| name == key.as_ref()) {
                        continue;
                    }
                    match splatted.iter_mut().find(|(name, _)| name == key.as_ref()) {
                        Some((_, existing)) => existing.push(Variant::new(cond, value.clone())),
                        None => splatted.push((
                            key.to_string(),
                            Variational::from_iter([Variant::new(cond, value.clone())]),
                        )),
                    }
                }
            }
            kw.extend(splatted);
        }

        Ok(CallArgs { pos, kw })
    }
}
