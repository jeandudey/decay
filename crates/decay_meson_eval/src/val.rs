use {
    crate::obj::Obj,
    decay_meson_logic::{Logic, Pc, Solver},
    eyre::bail,
    smallvec::{SmallVec, smallvec},
    std::{mem, rc::Rc},
};

/// A variational value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant<T> {
    /// The condition when this value is set.
    pub cond: Pc,
    /// The value.
    pub value: T,
}

impl<T> Variant<T> {
    /// A new [`Variant`].
    pub fn new(cond: Pc, value: T) -> Self {
        Self { cond, value }
    }
}

/// The variants of a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variational<T>(SmallVec<[Variant<T>; 1]>);

impl<T> Variational<T> {
    pub fn empty() -> Self {
        Self(SmallVec::new())
    }

    pub fn push(&mut self, variant: Variant<T>) {
        self.0.push(variant);
    }

    pub fn extend(&mut self, variants: impl Iterator<Item = Variant<T>>) {
        self.0.extend(variants);
    }

    pub fn normalize<S>(&mut self, logic: &mut Logic<S>)
    where
        T: Eq,
        S: Solver,
    {
        if self.0.len() <= 1 {
            return;
        }
        let mut out: SmallVec<[Variant<T>; 1]> = SmallVec::new();
        for v in self.0.drain(..) {
            if v.cond.is_false() {
                continue;
            }
            match out.iter_mut().find(|o| o.value == v.value) {
                Some(o) => o.cond = logic.or(o.cond, v.cond),
                None => out.push(v),
            }
        }
    }

    pub fn variants(&self) -> &[Variant<T>] {
        self.0.as_slice()
    }

    pub fn into_variants(self) -> smallvec::IntoIter<[Variant<T>; 1]> {
        self.0.into_iter()
    }
}

impl<T> Default for Variational<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T> From<Variant<T>> for Variational<T> {
    fn from(variant: Variant<T>) -> Self {
        Self(smallvec![variant])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Unset,
    Bool(bool),
    Str(Rc<str>),
    Int(i64),
    List(Rc<Vec<Variant<Value>>>),
    Dict(Rc<Vec<Variant<(Rc<str>, Value)>>>),
    Obj(Obj),
}

impl From<&'_ String> for Value {
    fn from(value: &'_ String) -> Self {
        Self::Str(Rc::from(value.as_str()))
    }
}

/*
impl Value {
    pub fn expect_string(&self) -> eyre::Result<String> {
        Ok(match self {
            Value::Str(v) => v.clone(),
            _ => bail!("expected a string"),
        })
    }

    pub fn expect_obj(&self) -> eyre::Result<Obj> {
        Ok(match self {
            Value::Obj(v) => v.clone(),
            _ => bail!("expected an object, found {self:?}"),
        })
    }
}
*/

/*
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarVal(Vec<(Pc, Value)>);

impl VarVal {
    pub fn from_string(ctx: Pc, v: impl Into<String>) -> Self {
        Self(vec![(ctx, Value::Str(v.into()))])
    }

    pub fn from_list(ctx: Pc, elements: Vec<VarVal>) -> Self {
        Self(vec![(ctx, Value::VarList(VarList(elements)))])
    }

    pub fn normalize<S: Solver>(&mut self, logic: &mut Logic<S>) {
        let mut out = Vec::new();
        for (pc, v) in mem::take(&mut self.0) {
            if pc == Pc::FALSE {
                continue;
            };
            match out.iter_mut().find(|(_, w)| v == *w) {
                Some(w) => w.0 = logic.or(w.0, pc),
                None => out.push((pc, v)),
            }
        }
        self.0 = out
            .into_iter()
            .filter(|&(pc, _)| logic.is_sat(pc))
            .collect();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarList(Vec<VarVal>);

#[derive(Debug, Clone)]
pub struct VarArgs {
    pub pos: Vec<VarVal>,
    pub kw: Vec<(String, VarVal)>,
}

impl VarArgs {
    pub fn split<S: Solver>(&self, logic: &mut Logic<S>, ctx: Pc) -> Vec<ConcreteArgs> {
        todo!()
    }
}

#[derive(Debug, Clone)]
pub struct ConcreteArgs {
    pub pos: Vec<Value>,
    pub kw: Vec<(String, Value)>,
    pub pc: Pc,
}

//pub fn array_lit<G: GuardCtx>(guard_ctx: &G, ctx: G::Id, elements: Vec<Val<G::Id>>) -> Val<G::Id> {
//    let is_pure = elements.iter().all(|v| v.is_pure());
//    if ctx == guard_ctx.top() && is_pure {
//        Val::Pure(Const::Array(
//            elements.into_iter().map(|v| v.unwrap_pure()).collect(),
//        ))
//    } else {
//        Val::Array(Array {
//            frags: vec![(ctx, elements)],
//        })
//    }
//}
*/
