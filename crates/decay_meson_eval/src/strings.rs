use {
    crate::{
        Interp,
        obj::Obj,
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
    ///
    /// A template that is *only* one hole (`f'@dir@'`, `'@0@'.format(dir)`)
    /// is meson's idiom for passing a value through unchanged — most often
    /// `meson.current_source_dir()` as a `command:` argument (fontconfig's
    /// `'@0@'.format(meson.current_source_dir())`, handed to a script's
    /// `-d` flag). `stringify()` below has to degrade an `Obj::File`/
    /// `Obj::PrefixedFile` to its bare path text (there is no way to splice
    /// a build-graph reference into an arbitrary position of a general
    /// template), which a `command:` argument can no longer resolve once the
    /// build runs elsewhere — so this case returns the value itself,
    /// preserving the reference `command()` already knows how to handle.
    pub(crate) fn format_string(&mut self, template: &str) -> eyre::Result<Variational<Value>> {
        let pieces = split_holes(template);
        if let [Piece::Hole(name)] = pieces.as_slice() {
            let value = self
                .lookup(name)?
                .ok_or_else(|| eyre::eyre!("`@{name}@` names an undefined variable"))?;
            if let [variant] = value.variants()
                && matches!(
                    variant.value,
                    Value::Obj(Obj::File(_) | Obj::PrefixedFile(..))
                )
            {
                return Ok(value);
            }
        }

        let mut out = Variational::from(Variant::new(self.pc, String::new()));

        for piece in pieces {
            let addition: Variational<Rc<str>> = match piece {
                Piece::Literal(text) => {
                    Variant::new(self.pc, Rc::from(text)).into() //
                }
                Piece::Hole(name) => {
                    let value = self
                        .lookup(name)?
                        .ok_or_else(|| eyre::eyre!("`@{name}@` names an undefined variable"))?;
                    self.stringify(&value)?
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
    ///
    /// See [`Self::format_string`]'s doc comment: a lone `'@0@'.format(x)`
    /// gets the same pass-through treatment for a `File`/`PrefixedFile` `x`.
    pub(crate) fn format_positional(
        &mut self,
        template: &str,
        args: &[Variational<Value>],
    ) -> eyre::Result<Variational<Value>> {
        let pieces = split_holes(template);
        if let [Piece::Hole(name)] = pieces.as_slice() {
            let index: usize = name
                .parse()
                .map_err(|_| eyre::eyre!("`@{name}@` is not a positional hole"))?;
            let arg = args
                .get(index)
                .ok_or_else(|| eyre::eyre!("`@{index}@` has no matching argument"))?;
            if let [variant] = arg.variants()
                && matches!(
                    variant.value,
                    Value::Obj(Obj::File(_) | Obj::PrefixedFile(..))
                )
            {
                return Ok(arg.clone());
            }
        }

        let mut out = Variational::from(Variant::new(self.pc, String::new()));

        for piece in pieces {
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
            out.push(Variant::new(variant.cond, text_of(&variant.value)?));
        }
        Ok(out)
    }
}

/// A value interpolated into a plain string (a compiler flag, `join_paths()`
/// segment, ...). A source-tree path has no build-graph reference to become
/// once it lands here, so this at least keeps the path meson would have
/// produced, same as before it was tracked instead of being a string.
pub(crate) fn text_of(value: &Value) -> eyre::Result<Rc<str>> {
    Ok(match value {
        Value::Str(s) => s.clone(),
        Value::Int(i) => Rc::from(i.to_string().as_str()),
        Value::Bool(b) => Rc::from(if *b { "true" } else { "false" }),
        Value::Obj(Obj::File(p)) => p.clone(),
        other => bail!("cannot interpolate a {}", other.type_name()),
    })
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
