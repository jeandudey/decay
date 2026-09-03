use {
    crate::{
        Interp,
        val::Value, //
    },
    decay_meson_logic::{
        Solver,
        Variant,
        Variational, //
    },
    eyre::bail,
    std::rc::Rc,
};

impl<'a, S: Solver> Interp<'a, S> {
    /// Expand an `f'...'` string, whose `@name@` holes read variables in scope.
    ///
    /// A hole filled by a value that differs between configurations makes the
    /// whole string differ, so the result is variational too.
    pub(crate) fn format_string(&mut self, template: &str) -> eyre::Result<Variational<Value>> {
        let mut out = Variational::from(Variant::new(self.pc, String::new()));

        for piece in split_holes(template) {
            let addition: Variational<Rc<str>> = match piece {
                Piece::Literal(text) => {
                    Variant::new(self.pc, Rc::from(text)).into() //
                }
                Piece::Hole(name) => {
                    let value = self
                        .lookup(name)?
                        .ok_or_else(|| eyre::eyre!("`@{name}@` names an undefined variable"))?;
                    self.strings(&value)?
                }
            };

            let mut next = Variational::empty();
            for base in out.variants() {
                for add in addition.variants() {
                    let cond = self.logic.and(base.cond, add.cond);
                    if cond.is_false() {
                        continue;
                    }
                    next.push(Variant::new(cond, format!("{}{}", base.value, add.value)));
                }
            }
            next.normalize(&mut self.logic);
            out = next;
        }

        let mut values = out.map(Value::from);
        values.normalize(&mut self.logic);
        Ok(values)
    }

    /// `'...@0@...'.format(a, b)`, where the holes are positional.
    pub(crate) fn format_positional(
        &mut self,
        template: &str,
        args: &[Variational<Value>],
    ) -> eyre::Result<Variational<Value>> {
        let mut out = Variational::from(Variant::new(self.pc, String::new()));

        for piece in split_holes(template) {
            let addition: Variational<Rc<str>> = match piece {
                Piece::Literal(text) => Variant::new(self.pc, Rc::from(text)).into(),
                Piece::Hole(name) => {
                    let index: usize = name
                        .parse()
                        .map_err(|_| eyre::eyre!("`@{name}@` is not a positional hole"))?;
                    let arg = args
                        .get(index)
                        .ok_or_else(|| eyre::eyre!("`@{index}@` has no matching argument"))?;
                    self.stringify(arg)?
                }
            };

            let mut next = Variational::empty();
            for base in out.variants() {
                for add in addition.variants() {
                    let cond = self.logic.and(base.cond, add.cond);
                    if cond.is_false() {
                        continue;
                    }
                    next.push(Variant::new(cond, format!("{}{}", base.value, add.value)));
                }
            }
            next.normalize(&mut self.logic);
            out = next;
        }

        let mut values = out.map(Value::from);
        values.normalize(&mut self.logic);
        Ok(values)
    }

    /// Render a value the way meson's string interpolation does.
    pub(crate) fn stringify(
        &mut self,
        v: &Variational<Value>,
    ) -> eyre::Result<Variational<Rc<str>>> {
        let mut out = Variational::empty();
        for variant in v.variants() {
            let text: Rc<str> = match &variant.value {
                Value::Str(s) => s.clone(),
                Value::Int(i) => Rc::from(i.to_string().as_str()),
                Value::Bool(b) => Rc::from(if *b { "true" } else { "false" }),
                other => bail!("cannot interpolate a {}", other.type_name()),
            };
            out.push(Variant::new(variant.cond, text));
        }
        Ok(out)
    }
}

enum Piece<'a> {
    Literal(&'a str),
    Hole(&'a str),
}

/// Split on `@name@`, leaving anything that does not close as literal text.
fn split_holes(template: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = template;

    while let Some(start) = rest.find('@') {
        let (before, after) = rest.split_at(start);
        let body = &after[1..];
        match body.find('@') {
            Some(end) if end > 0 && body[..end].chars().all(is_hole_char) => {
                if !before.is_empty() {
                    out.push(Piece::Literal(before));
                }
                out.push(Piece::Hole(&body[..end]));
                rest = &body[end + 1..];
            }
            _ => {
                out.push(Piece::Literal(&rest[..start + 1]));
                rest = body;
            }
        }
    }

    if !rest.is_empty() {
        out.push(Piece::Literal(rest));
    }
    out
}

fn is_hole_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
