use {
    crate::{
        arena::Pc,
        logic::Logic,
        solver::Solver, //
    },
    smallvec::{
        SmallVec,
        smallvec, //
    },
};

/// One possible value, together with the configurations it appears in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Variant<T> {
    /// The condition under which the value is this one.
    pub cond: Pc,
    pub value: T,
}

impl<T> Variant<T> {
    pub fn new(cond: Pc, value: T) -> Self {
        Self { cond, value }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Variant<U> {
        Variant {
            cond: self.cond,
            value: f(self.value),
        }
    }

    pub fn as_ref(&self) -> Variant<&T> {
        Variant {
            cond: self.cond,
            value: &self.value,
        }
    }
}

/// A value that may differ between configurations.
///
/// The variants are meant to be mutually exclusive and, taken together, to
/// cover the path condition they were produced under. An empty `Variational` is
/// a value that exists in no configuration at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variational<T>(SmallVec<[Variant<T>; 1]>);

impl<T> Variational<T> {
    pub fn empty() -> Self {
        Self(SmallVec::new())
    }

    pub fn pure(value: T) -> Self {
        Self(smallvec![Variant::new(Pc::TRUE, value)])
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn push(&mut self, variant: Variant<T>) {
        if variant.cond.is_false() {
            return;
        }
        self.0.push(variant);
    }

    pub fn extend(&mut self, variants: impl IntoIterator<Item = Variant<T>>) {
        for v in variants {
            self.push(v);
        }
    }

    pub fn variants(&self) -> &[Variant<T>] {
        self.0.as_slice()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Variant<T>> {
        self.0.iter()
    }

    pub fn into_variants(self) -> smallvec::IntoIter<[Variant<T>; 1]> {
        self.0.into_iter()
    }

    /// The single variant, when the value does not vary at all.
    pub fn as_single(&self) -> Option<&T> {
        match self.0.as_slice() {
            [v] if v.cond.is_true() => Some(&v.value),
            _ => None,
        }
    }

    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Variational<U> {
        Variational(self.0.into_iter().map(|v| v.map(&mut f)).collect())
    }

    /// Narrow every variant to `pc`, dropping the ones that become impossible.
    pub fn restrict<S: Solver>(&self, logic: &mut Logic<S>, pc: Pc) -> Self
    where
        T: Clone,
    {
        let mut out = Self::empty();
        for v in &self.0 {
            let cond = logic.and(pc, v.cond);
            if cond.is_false() {
                continue;
            }
            out.push(Variant::new(cond, v.value.clone()));
        }
        out
    }

    /// The condition under which the value exists at all.
    pub fn domain<S: Solver>(&self, logic: &mut Logic<S>) -> Pc {
        let mut out = Pc::FALSE;
        for v in &self.0 {
            out = logic.or(out, v.cond);
        }
        out
    }

    /// Fuse variants that carry equal values and drop unreachable ones.
    ///
    /// Without this, a value that is written the same way on both sides of an
    /// `if` would keep two variants forever and the branch structure would leak
    /// into everything downstream.
    pub fn normalize<S: Solver>(&mut self, logic: &mut Logic<S>)
    where
        T: Eq,
    {
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
        out.retain(|v| logic.is_sat(v.cond));
        self.0 = out;
    }
}

impl<T> Default for Variational<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T> From<Variant<T>> for Variational<T> {
    fn from(variant: Variant<T>) -> Self {
        if variant.cond.is_false() {
            return Self::empty();
        }
        Self(smallvec![variant])
    }
}

impl<T> FromIterator<Variant<T>> for Variational<T> {
    fn from_iter<I: IntoIterator<Item = Variant<T>>>(iter: I) -> Self {
        let mut out = Self::empty();
        out.extend(iter);
        out
    }
}

impl<T> IntoIterator for Variational<T> {
    type Item = Variant<T>;
    type IntoIter = smallvec::IntoIter<[Variant<T>; 1]>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Variational<T> {
    type Item = &'a Variant<T>;
    type IntoIter = std::slice::Iter<'a, Variant<T>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
