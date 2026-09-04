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
        Solver,
        Variant,
        Variational, //
    },
    eyre::bail,
    std::rc::Rc,
};

impl<'a, S: Solver> Interp<'a, S> {
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
        let strish = |v: &Value| matches!(v, Value::Str(_) | Value::StrCat(_));
        if !lhs.is_empty()
            && !rhs.is_empty()
            && lhs.variants().iter().all(|v| strish(&v.value))
            && rhs.variants().iter().all(|v| strish(&v.value))
        {
            // A string built up from conditional fragments stays one value
            // carrying its pieces, so `x = x + fragment` under a chain of
            // `if`s is linear rather than `2^n`.
            let pc = self.pc;
            let mut pieces = self.str_pieces(lhs);
            pieces.extend(self.str_pieces(rhs));
            pieces.retain_mut(|p| {
                // A piece the current path already guarantees carries no
                // condition of its own here; one the path rules out drops.
                // Without this reduction `'HAVE_' + x.to_upper()` would defer
                // as a `StrCat` just because the two halves came back tagged
                // with structurally different (but equivalent) conditions.
                if p.cond == pc || p.cond.is_true() {
                    p.cond = Pc::TRUE;
                    return true;
                }
                if self.logic.and(pc, p.cond).is_false() {
                    return false;
                }
                if self.logic.entails(pc, p.cond) {
                    p.cond = Pc::TRUE;
                }
                true
            });
            return Ok(self.pure(Value::str_cat(pieces)));
        }

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

    /// Flatten string-ish variants into ordered conditional pieces. A piece's
    /// condition is relative to the value existing at all — the caller's path
    /// condition is not folded in here, the same way [`Value::List`] entries
    /// carry only their own condition.
    fn str_pieces(&mut self, v: &Variational<Value>) -> Vec<Variant<Rc<str>>> {
        let mut out = Vec::new();
        for variant in v.variants() {
            if variant.cond.is_false() {
                continue;
            }
            match &variant.value {
                Value::Str(s) => out.push(Variant::new(variant.cond, s.clone())),
                // Pieces already carry their own condition. When the whole
                // value is present on exactly the current path — the usual
                // case straight out of `pure` — they pass through untouched;
                // re-`and`ing each one is what turned `x = x + fragment` in a
                // loop into `O(n^2)` fresh arena nodes.
                Value::StrCat(pieces) if variant.cond == self.pc || variant.cond.is_true() => {
                    out.extend(pieces.iter().cloned());
                }
                Value::StrCat(pieces) => {
                    for p in pieces.iter() {
                        let c = self.logic.and(variant.cond, p.cond);
                        if !c.is_false() {
                            out.push(Variant::new(c, p.value.clone()));
                        }
                    }
                }
                // `add` only calls this once both sides are all string-ish.
                _ => {}
            }
        }
        out
    }

    /// [`Self::str_pieces`], but under an explicit condition folded into every
    /// piece — the string analogue of [`Interp::elements_under`], for growing a
    /// string variable in place.
    pub(crate) fn str_pieces_under(
        &mut self,
        v: &Variational<Value>,
        pc: Pc,
    ) -> Vec<Variant<Rc<str>>> {
        let mut out = Vec::new();
        for variant in v.variants() {
            let cond = self.logic.and(pc, variant.cond);
            if cond.is_false() {
                continue;
            }
            match &variant.value {
                Value::Str(s) => out.push(Variant::new(cond, s.clone())),
                Value::StrCat(pieces) => {
                    for p in pieces.iter() {
                        let c = self.logic.and(cond, p.cond);
                        if !c.is_false() {
                            out.push(Variant::new(c, p.value.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
        out
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
