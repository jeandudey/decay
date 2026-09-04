use {
    crate::{
        Interp,
        obj::Obj,
        val::Value, //
    },
    decay_meson_ast::{
        BinOp,
        BinOpKind, //
    },
    decay_meson_logic::{
        Pc,
        Variant,
        Variational, //
    },
    eyre::bail,
    std::rc::Rc,
};

impl<'a> Interp<'a> {
    pub(crate) fn binop(&mut self, op: &BinOp) -> eyre::Result<Variational<Value>> {
        // `and`/`or` short-circuit, and here that is not just an optimisation:
        // the right-hand side is often only well-defined when the left one
        // held, so it has to be evaluated under that condition.
        if matches!(op.kind, BinOpKind::And | BinOpKind::Or) {
            let lhs = self.expr(&op.lhs)?;
            let l = self.truth(&lhs)?;
            let guard = match op.kind {
                BinOpKind::And => l,
                _ => self.logic.not(l),
            };
            let rhs_pc = self.logic.and(self.pc, guard);
            let r = if rhs_pc.is_false() {
                Pc::FALSE
            } else {
                let rhs = self.with_pc(rhs_pc, |this| this.expr(&op.rhs))?;
                self.truth(&rhs)?
            };
            let cond = match op.kind {
                BinOpKind::And => self.logic.and(l, r),
                _ => self.logic.or(l, r),
            };
            return Ok(self.bool_value(cond));
        }

        let lhs = self.expr(&op.lhs)?;
        let rhs = self.expr(&op.rhs)?;

        match op.kind {
            BinOpKind::Eq | BinOpKind::Ne => {
                let eq = self.equals(&lhs, &rhs)?;
                let cond = if op.kind == BinOpKind::Ne {
                    self.logic.not(eq)
                } else {
                    eq
                };
                Ok(self.bool_value(cond))
            }
            BinOpKind::In | BinOpKind::NotIn => {
                let inside = self.contains(&rhs, &lhs)?;
                let cond = if op.kind == BinOpKind::NotIn {
                    self.logic.not(inside)
                } else {
                    inside
                };
                Ok(self.bool_value(cond))
            }
            BinOpKind::Add => self.add(&lhs, &rhs),
            kind => self.map2(&lhs, &rhs, |a, b| arith(kind, a, b)),
        }
    }

    /// `a + b`, which in meson means concatenation for lists and strings,
    /// merging for dicts, and arithmetic for numbers.
    pub(crate) fn add(
        &mut self,
        lhs: &Variational<Value>,
        rhs: &Variational<Value>,
    ) -> eyre::Result<Variational<Value>> {
        if lhs.variants().iter().any(|v| v.value.is_list()) {
            // Concatenating conditional lists element-wise keeps the result a
            // single list, so a chain of `if`s appending to one variable does
            // not multiply out into a variant per combination.
            let mut items = self.elements(lhs);
            items.extend(self.elements(rhs));
            return Ok(self.pure(Value::list(items)));
        }

        if lhs
            .variants()
            .iter()
            .any(|v| matches!(v.value, Value::Dict(_)))
        {
            let mut items: Vec<Variant<(Rc<str>, Value)>> = Vec::new();
            for side in [lhs, rhs] {
                for variant in side.variants() {
                    let cond = self.logic.and(self.pc, variant.cond);
                    if cond.is_false() {
                        continue;
                    }
                    let Value::Dict(entries) = &variant.value else {
                        bail!("cannot add a {} to a dict", variant.value.type_name());
                    };
                    for entry in entries.iter() {
                        let c = self.logic.and(cond, entry.cond);
                        if c.is_false() {
                            continue;
                        }
                        items.push(Variant::new(c, entry.value.clone()));
                    }
                }
            }
            return Ok(self.pure(Value::dict(items)));
        }

        self.map2(lhs, rhs, |a, b| arith(BinOpKind::Add, a, b))
    }

    /// The configurations in which two values are equal.
    pub(crate) fn equals(
        &mut self,
        lhs: &Variational<Value>,
        rhs: &Variational<Value>,
    ) -> eyre::Result<Pc> {
        let mut out = Pc::FALSE;
        for a in lhs.variants() {
            for b in rhs.variants() {
                if a.value != b.value {
                    continue;
                }
                let both = self.logic.and(a.cond, b.cond);
                out = self.logic.or(out, both);
            }
        }
        Ok(self.logic.and(self.pc, out))
    }

    /// The configurations in which `needle` is an element of `haystack`.
    fn contains(
        &mut self,
        haystack: &Variational<Value>,
        needle: &Variational<Value>,
    ) -> eyre::Result<Pc> {
        let items = self.flat(haystack);
        let mut out = Pc::FALSE;
        for item in &items {
            for n in needle.variants() {
                if item.value != n.value {
                    continue;
                }
                let both = self.logic.and(item.cond, n.cond);
                out = self.logic.or(out, both);
            }
        }
        Ok(out)
    }

    /// Apply a binary function across the product of both sides' variants.
    pub(crate) fn map2(
        &mut self,
        lhs: &Variational<Value>,
        rhs: &Variational<Value>,
        mut f: impl FnMut(&Value, &Value) -> eyre::Result<Value>,
    ) -> eyre::Result<Variational<Value>> {
        let mut out = Variational::empty();
        for a in lhs.variants() {
            for b in rhs.variants() {
                let cond = self.logic.and(a.cond, b.cond);
                if cond.is_false() {
                    continue;
                }
                out.push(Variant::new(cond, f(&a.value, &b.value)?));
            }
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }
}

fn arith(kind: BinOpKind, a: &Value, b: &Value) -> eyre::Result<Value> {
    use BinOpKind::*;
    Ok(match (kind, a, b) {
        (Add, Value::Int(a), Value::Int(b)) => Value::Int(a + b),
        (Sub, Value::Int(a), Value::Int(b)) => Value::Int(a - b),
        (Mul, Value::Int(a), Value::Int(b)) => Value::Int(a * b),
        (Div, Value::Int(a), Value::Int(b)) if *b != 0 => Value::Int(a / b),
        (Mod, Value::Int(a), Value::Int(b)) if *b != 0 => Value::Int(a % b),
        (Div | Mod, Value::Int(_), Value::Int(_)) => bail!("division by zero"),
        (Add, Value::Str(a), Value::Str(b)) => Value::str(format!("{a}{b}")),
        // Meson overloads `/` on strings as path joining.
        (Div, Value::Str(a), Value::Str(b)) => Value::str(join_paths([&**a, &**b])),
        // Joining onto a source-tree path stays a reference to the tree,
        // rather than decaying into a plain string that means nothing by the
        // time a command runs.
        (Div, Value::Obj(Obj::File(a)), Value::Str(b)) => {
            Value::Obj(Obj::File(Rc::from(join_paths([&**a, &**b]).as_str())))
        }
        (Lt, a, b) => Value::Bool(compare(a, b)? == std::cmp::Ordering::Less),
        (Le, a, b) => Value::Bool(compare(a, b)? != std::cmp::Ordering::Greater),
        (Gt, a, b) => Value::Bool(compare(a, b)? == std::cmp::Ordering::Greater),
        (Ge, a, b) => Value::Bool(compare(a, b)? != std::cmp::Ordering::Less),
        (kind, a, b) => bail!(
            "cannot apply `{kind}` to a {} and a {}",
            a.type_name(),
            b.type_name()
        ),
    })
}

fn compare(a: &Value, b: &Value) -> eyre::Result<std::cmp::Ordering> {
    Ok(match (a, b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Str(a), Value::Str(b)) => a.as_ref().cmp(b.as_ref()),
        (a, b) => bail!(
            "cannot compare a {} with a {}",
            a.type_name(),
            b.type_name()
        ),
    })
}

/// Meson's `join_paths`: an absolute segment discards everything before it.
pub(crate) fn join_paths<'s>(segments: impl IntoIterator<Item = &'s str>) -> String {
    let mut out = String::new();
    for segment in segments {
        if segment.is_empty() {
            continue;
        }
        if segment.starts_with('/') || out.is_empty() {
            out.clear();
            out.push_str(segment);
        } else {
            if !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(segment);
        }
    }
    out
}
