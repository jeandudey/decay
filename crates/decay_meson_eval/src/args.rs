use {
    crate::{
        Interp,
        val::Value, //
    },
    decay_meson_ast::Args,
    decay_meson_logic::{
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

impl<'a> Interp<'a> {
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

        Ok(CallArgs { pos, kw })
    }
}
