use {
    crate::stats,
    biodivine_lib_bdd::{
        Bdd,
        BddVariable,
        BddVariableSet, //
    },
    smallvec::SmallVec,
    std::collections::HashMap,
};

/// BDD variables reserved per priority band (see [`band`]). Each configuration
/// variable claims `ceil(log2(choices))` of its band's slots; a band that runs
/// out trips the panic in [`Arena::declare`] — the signal to raise this (the
/// hard ceiling is `4 * BAND <= u16::MAX - 1`).
const BAND: u16 = 4096;

/// Number of priority bands.
const BANDS: usize = 4;

/// Which band a variable's BDD bits go in. Lower band = closer to the root of
/// every diagram, so `decay_buck2::select` branches on it first: a project's
/// own options before probe results before the target machine before an
/// externally-named constraint — the order a hand-written `select()` would
/// nest.
fn band(kind: VarKind) -> usize {
    match kind {
        VarKind::Option | VarKind::BuiltinOption => 0,
        VarKind::Probe | VarKind::Dependency => 1,
        VarKind::Machine => 2,
        VarKind::Constraint => 3,
    }
}

/// A presence condition: an index into an [`Arena`], which now stores each
/// distinct condition as a reduced ordered BDD.
///
/// Conditions are built over *multi-valued* variables (a meson `combo` option,
/// the host system, ...). The atom `lit(v, i)` reads "variable `v` took its
/// `i`th choice"; the BDD encodes each such variable's choice index in
/// `ceil(log2(n))` boolean BDD variables. Keeping the choice structure is what
/// lets the backend lower a condition to nested `select()`s whose keys are
/// mutually exclusive by construction.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Pc(u32);

impl Pc {
    /// Also the [`Default`], so a freshly built structure starts out present in
    /// no configuration until something says otherwise.
    pub const FALSE: Self = Self(0);
    pub const TRUE: Self = Self(1);

    pub fn from_bool(value: bool) -> Self {
        if value { Self::TRUE } else { Self::FALSE }
    }

    pub fn is_false(&self) -> bool {
        *self == Self::FALSE
    }

    pub fn is_true(&self) -> bool {
        *self == Self::TRUE
    }

    pub fn is_const(&self) -> bool {
        self.is_true() || self.is_false()
    }

    pub fn index(&self) -> u32 {
        self.0
    }
}

/// A configuration variable.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VarId(u32);

impl VarId {
    pub fn index(&self) -> usize {
        self.0 as usize
    }

    /// Rebuild an id from a position in [`Arena::vars`], for callers that were
    /// handed the variable list rather than the arena.
    pub fn from_index(index: usize) -> Self {
        Self(index as u32)
    }
}

/// What a configuration variable stands for.
///
/// The backend uses this to decide how a variable is materialized: a build
/// option becomes a user-facing constraint, a probe becomes an environment
/// capability flag, and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarKind {
    /// An option the project declares in its own option file.
    Option,
    /// One of meson's own build options, which every project has and none
    /// declares.
    BuiltinOption,
    /// A property of the machine being built for.
    Machine,
    /// The result of a toolchain/environment probe (`cc.has_header`, ...).
    Probe,
    /// Whether an external dependency is available.
    Dependency,
    /// A constraint from outside the importer, named in its configuration and
    /// selected on directly. The importer knows only the values that
    /// configuration mentions, so [`ANY_OTHER`] stands for the rest.
    Constraint,
}

/// The choice standing for "any value nobody named".
///
/// A constraint the importer was handed a label for is not one it declares, so
/// it cannot know every value the setting has. The values the configuration
/// mentions are choices of their own and everything else is this one lump,
/// which is what a `select()` renders as `DEFAULT`. Not a legal buck2 value
/// name, so it cannot collide with a real one.
pub const ANY_OTHER: &str = "*";

/// The declaration of a configuration variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    /// Unique key, e.g. `option:glx`.
    pub key: String,
    /// Human readable description, when the meson sources provided one.
    pub description: Option<String>,
    pub kind: VarKind,
    /// The values the variable may take. Always at least two.
    pub choices: Vec<String>,
    /// Index into `choices` of the value meson would pick by default.
    pub default: usize,
}

impl Var {
    pub fn choice_index(&self, name: &str) -> Option<u32> {
        self.choices.iter().position(|c| c == name).map(|i| i as u32)
    }
}

/// Bits needed to number `n` choices: `ceil(log2(n))`, at least 1.
fn width(n: u32) -> u8 {
    debug_assert!(n >= 2);
    (32 - (n - 1).leading_zeros()).max(1) as u8
}

/// Store of presence conditions, each a canonical BDD. Equivalent conditions —
/// however they were built — share one [`Pc`], so the volume of distinct
/// entries tracks the real branching structure of a project, not how many
/// questions were asked about it.
#[derive(Debug)]
pub struct Arena {
    vs: BddVariableSet,
    vars: Vec<Var>,
    by_key: HashMap<String, VarId>,
    /// `bits[v]` = (first BDD variable, count) encoding configuration variable
    /// `v`. Dense, in `VarId` order.
    bits: Vec<(u16, u8)>,
    /// Next free BDD variable in each priority band.
    next_bit: [u16; BANDS],
    /// `Pc` -> its canonical BDD. Slots 0 and 1 are `FALSE` / `TRUE`.
    bdds: Vec<Bdd>,
    dedup: HashMap<Bdd, Pc>,
    /// Cached [`Self::support`] results — a pure function of the BDD, so safe to
    /// memoise per `Pc`.
    support_memo: HashMap<Pc, Vec<VarId>>,
    /// Domain constraints plus everything [`assume`](Self::assume)d: the
    /// configurations that exist at all.
    context: Bdd,
    restrict_memo: HashMap<(Pc, VarId, u32), Pc>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Arena {
    pub fn new() -> Self {
        let vs = BddVariableSet::new_anonymous(BAND * BANDS as u16);
        let (f, t) = (vs.mk_false(), vs.mk_true());
        let mut dedup = HashMap::new();
        dedup.insert(f.clone(), Pc::FALSE);
        dedup.insert(t.clone(), Pc::TRUE);
        Self {
            context: vs.mk_true(),
            vs,
            vars: Vec::new(),
            by_key: HashMap::new(),
            bits: Vec::new(),
            next_bit: std::array::from_fn(|b| b as u16 * BAND),
            bdds: vec![f, t],
            dedup,
            support_memo: HashMap::new(),
            restrict_memo: HashMap::new(),
        }
    }

    pub fn vars(&self) -> &[Var] {
        &self.vars
    }

    pub fn var(&self, id: VarId) -> &Var {
        &self.vars[id.0 as usize]
    }

    pub fn var_id(&self, key: &str) -> Option<VarId> {
        self.by_key.get(key).copied()
    }

    /// The BDD node count of one condition — how big it actually got.
    pub fn size(&self, pc: Pc) -> usize {
        self.bdds[pc.0 as usize].size()
    }

    /// Declare a variable, or return the existing one with the same key.
    ///
    /// Re-declaring with a different domain is a bug in the caller rather than
    /// something the sources can trigger, so the first declaration wins.
    pub fn declare(&mut self, var: Var) -> VarId {
        if let Some(id) = self.by_key.get(&var.key) {
            return *id;
        }
        let n = var.choices.len() as u32;
        debug_assert!(n >= 2, "`{}` has no real choice", var.key);

        let id = VarId(self.vars.len() as u32);
        let b = band(var.kind);
        self.by_key.insert(var.key.clone(), id);
        self.vars.push(var);
        stats::max(&stats::ARENA_VARS, self.vars.len() as u64);
        stats::bump(&stats::VAR_DECLARE);

        let count = width(n);
        let start = self.next_bit[b];
        let end = start + u16::from(count);
        assert!(
            end <= (b as u16 + 1) * BAND,
            "BDD variable band {b} exhausted at {BAND}; raise `BAND` in arena.rs",
        );
        self.next_bit[b] = end;
        self.bits.push((start, count));

        // Rule out the bit patterns past `n` when it is not a power of two, so
        // a variable can only ever hold a value it actually has.
        if !n.is_power_of_two() {
            let mut legal = self.vs.mk_false();
            for value in 0..n {
                legal = legal.or(&self.encode(start, count, value));
            }
            self.context = self.context.and(&legal);
        }
        id
    }

    pub fn lit(&mut self, var: VarId, choice: u32) -> Pc {
        let (start, count) = self.bits[var.index()];
        debug_assert!(choice < (1u32 << count));
        let bdd = self.encode(start, count, choice);
        self.intern(bdd)
    }

    pub fn not(&mut self, a: Pc) -> Pc {
        stats::bump(&stats::ARENA_NOT_CALLS);
        if a.is_true() {
            return Pc::FALSE;
        }
        if a.is_false() {
            return Pc::TRUE;
        }
        let bdd = self.bdds[a.0 as usize].not();
        self.intern(bdd)
    }

    pub fn and(&mut self, a: Pc, b: Pc) -> Pc {
        stats::bump(&stats::ARENA_AND_CALLS);
        if a.is_false() || b.is_false() {
            return Pc::FALSE;
        }
        if a.is_true() {
            return b;
        }
        if b.is_true() || a == b {
            return a;
        }
        let bdd = self.bdds[a.0 as usize].and(&self.bdds[b.0 as usize]);
        self.intern(bdd)
    }

    pub fn or(&mut self, a: Pc, b: Pc) -> Pc {
        stats::bump(&stats::ARENA_OR_CALLS);
        if a.is_true() || b.is_true() {
            return Pc::TRUE;
        }
        if a.is_false() {
            return b;
        }
        if b.is_false() || a == b {
            return a;
        }
        let bdd = self.bdds[a.0 as usize].or(&self.bdds[b.0 as usize]);
        self.intern(bdd)
    }

    pub fn implies(&mut self, a: Pc, b: Pc) -> Pc {
        let na = self.not(a);
        self.or(na, b)
    }

    /// The value of `pc` once `var` is known to have taken `choice`.
    pub fn restrict(&mut self, pc: Pc, var: VarId, choice: u32) -> Pc {
        if pc.is_const() {
            return pc;
        }
        if let Some(&hit) = self.restrict_memo.get(&(pc, var, choice)) {
            return hit;
        }
        let (start, count) = self.bits[var.index()];
        let assignment: SmallVec<[(BddVariable, bool); 6]> = (0..count)
            .map(|i| {
                (
                    BddVariable::from_index((start + u16::from(i)) as usize),
                    (choice >> i) & 1 == 1,
                )
            })
            .collect();
        let bdd = self.bdds[pc.0 as usize].restrict(&assignment);
        let out = self.intern(bdd);
        self.restrict_memo.insert((pc, var, choice), out);
        out
    }

    /// The variables `pc` actually mentions, in declaration order.
    ///
    /// `decay_buck2::select` takes a branching variable off the front of this,
    /// so it must be a stable function of the condition — reading it straight
    /// off the (canonical) BDD's support is exactly that.
    pub fn support(&mut self, pc: Pc) -> Vec<VarId> {
        if let Some(hit) = self.support_memo.get(&pc) {
            return hit.clone();
        }
        // Order by first BDD bit, i.e. by priority band then declaration order
        // within a band — the order the diagram itself branches, so it matches
        // how a `select()` over these variables should nest.
        let mut vars: Vec<VarId> = Vec::new();
        for b in self.bdds[pc.0 as usize].support_set() {
            let bit = b.to_index() as u16;
            let v = self
                .bits
                .iter()
                .position(|&(start, count)| start <= bit && bit < start + u16::from(count))
                .map(VarId::from_index)
                .expect("every BDD variable belongs to a configuration variable");
            if !vars.contains(&v) {
                vars.push(v);
            }
        }
        vars.sort_unstable_by_key(|v| self.bits[v.index()].0);
        self.support_memo.insert(pc, vars.clone());
        vars
    }

    /// Narrow the whole configuration space by `pc`: from now on every
    /// assignment that falsifies it simply does not exist.
    pub fn assume(&mut self, pc: Pc) {
        self.context = self.context.and(&self.bdds[pc.0 as usize]);
    }

    /// Whether `pc` holds in some configuration that exists.
    pub fn is_sat(&self, pc: Pc) -> bool {
        !self.bdds[pc.0 as usize].and(&self.context).is_false()
    }

    /// The BDD for "the `count` bits from `start` spell `value`", LSB first.
    fn encode(&self, start: u16, count: u8, value: u32) -> Bdd {
        let mut b = self.vs.mk_true();
        for i in 0..count {
            let v = BddVariable::from_index((start + u16::from(i)) as usize);
            b = b.and(&self.vs.mk_literal(v, (value >> i) & 1 == 1));
        }
        b
    }

    fn intern(&mut self, bdd: Bdd) -> Pc {
        if bdd.is_false() {
            return Pc::FALSE;
        }
        if bdd.is_true() {
            return Pc::TRUE;
        }
        if let Some(&pc) = self.dedup.get(&bdd) {
            return pc;
        }
        let pc = Pc(self.bdds.len() as u32);
        let size = bdd.size() as u64;
        stats::bump(&stats::ARENA_INTERNED);
        stats::add(&stats::BDD_NODES_TOTAL, size);
        stats::max(&stats::BDD_MAX_SIZE, size);
        stats::max(&stats::PC_COUNT, pc.0 as u64 + 1);
        self.dedup.insert(bdd.clone(), pc);
        self.bdds.push(bdd);
        pc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(key: &str, choices: usize) -> Var {
        Var {
            key: key.to_owned(),
            description: None,
            kind: VarKind::Option,
            choices: (0..choices).map(|i| i.to_string()).collect(),
            default: 0,
        }
    }

    /// An arena with `n` variables of the given choice counts, `v0..`.
    fn arena(choices: &[usize]) -> (Arena, Vec<VarId>) {
        let mut a = Arena::new();
        let ids = choices
            .iter()
            .enumerate()
            .map(|(i, &n)| a.declare(var(&format!("v{i}"), n)))
            .collect();
        (a, ids)
    }

    #[test]
    fn width_is_ceil_log2() {
        assert_eq!(width(2), 1);
        assert_eq!(width(3), 2);
        assert_eq!(width(4), 2);
        assert_eq!(width(5), 3);
        assert_eq!(width(8), 3);
        assert_eq!(width(9), 4);
    }

    #[test]
    fn every_choice_of_a_variable_is_reachable() {
        let (mut a, v) = arena(&[3]);
        for choice in 0..3 {
            let lit = a.lit(v[0], choice);
            assert!(a.is_sat(lit), "choice {choice}");
        }
        let _ = &v;
    }

    #[test]
    fn choices_are_mutually_exclusive_and_the_spare_pattern_is_dead() {
        let (mut a, v) = arena(&[3]);
        let c0 = a.lit(v[0], 0);
        let c1 = a.lit(v[0], 1);
        let both = a.and(c0, c1);
        assert!(both.is_false(), "one variable, two values");
        // 3 choices uses 2 bits; the 4th pattern must not exist.
        let spare = a.lit(v[0], 3);
        assert!(!a.is_sat(spare));
    }

    #[test]
    fn equivalent_conditions_share_one_pc() {
        let (mut a, v) = arena(&[2, 2, 2]);
        let (x, y, z) = (a.lit(v[0], 1), a.lit(v[1], 1), a.lit(v[2], 1));
        let left = {
            let xy = a.and(x, y);
            a.and(xy, z)
        };
        let right = {
            let yz = a.and(y, z);
            a.and(x, yz)
        };
        assert_eq!(left, right, "(x&y)&z and x&(y&z) are one condition");
    }

    #[test]
    fn support_is_declaration_order_regardless_of_assembly() {
        let (mut a, v) = arena(&[2, 2, 2]);
        let (x, y, z) = (a.lit(v[0], 1), a.lit(v[1], 1), a.lit(v[2], 1));
        // "z & x & y" assembled two ways gives one condition and one support.
        let one = {
            let zx = a.and(z, x);
            a.and(zx, y)
        };
        let two = {
            let xy = a.and(x, y);
            a.and(xy, z)
        };
        assert_eq!(one, two);
        assert_eq!(a.support(one), vec![v[0], v[1], v[2]]);
    }

    #[test]
    fn assume_removes_configurations() {
        let (mut a, v) = arena(&[2, 2]);
        let x0 = a.lit(v[0], 0);
        a.assume(x0);
        let x1 = a.lit(v[0], 1);
        let y1 = a.lit(v[1], 1);
        assert!(!a.is_sat(x1), "x==1 assumed away");
        assert!(a.is_sat(y1), "y still free");
    }

    #[test]
    fn restrict_substitutes_a_choice() {
        let (mut a, v) = arena(&[2, 2]);
        let x1 = a.lit(v[0], 1);
        let y1 = a.lit(v[1], 1);
        let both = a.and(x1, y1);
        let rx1 = a.restrict(both, v[0], 1);
        assert_eq!(rx1, y1, "x==1 makes it just y==1");
        assert!(a.restrict(both, v[0], 0).is_false(), "x==0 kills it");
        assert_eq!(a.support(rx1), vec![v[1]]);
    }
}
