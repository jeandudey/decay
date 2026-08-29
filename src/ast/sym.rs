use {
    eyre::{OptionExt, bail},
    std::{
        cell::RefCell,
        collections::HashMap, //
    },
    z3::{
        Context, SatResult, Solver,
        ast::{Bool, Int},
    },
};

pub type SettingId = u32;

#[derive(Debug, Clone)]
pub struct Setting {
    pub key: String,
    pub truthy: Option<usize>,
    pub choices: Vec<String>,
}

#[derive(Debug)]
pub struct Env {
    ctx: Context,
    solver: Solver,
    consts: RefCell<Vec<Int>>,
    index: RefCell<HashMap<String, SettingId>>,
    settings: RefCell<Vec<Setting>>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            ctx: Context::thread_local(),
            solver: Solver::new(),
            consts: RefCell::new(Vec::new()),
            index: RefCell::new(HashMap::new()),
            settings: RefCell::new(Vec::new()),
        }
    }

    pub fn setting_id(&self, key: &str) -> Option<SettingId> {
        self.index.borrow().get(key).copied()
    }

    pub fn setting(&self, id: SettingId) -> eyre::Result<Setting> {
        self.settings
            .borrow()
            .get(id as usize)
            .cloned()
            .ok_or_eyre("Invalid setting ID")
    }

    pub fn n_choices(&self, id: SettingId) -> usize {
        self.settings
            .borrow()
            .get(id as usize)
            .unwrap()
            .choices
            .len()
    }

    pub fn choice_index(&self, id: SettingId, name: &str) -> Option<u32> {
        self.settings.borrow()[id as usize]
            .choices
            .iter()
            .position(|c| c == name)
            .map(|i| i as u32)
    }

    pub fn intern(&self, setting: Setting) -> eyre::Result<SettingId> {
        if let Some(id) = self.setting_id(&setting.key) {
            return Ok(id);
        }

        if setting.choices.len() < 2 {
            bail!("setting has to little choices")
        }

        let id = self.settings.borrow().len() as SettingId;
        let c = Int::new_const(format!("s{id}"));
        let zero = Int::from_i64(0);
        let n = Int::from_i64(setting.choices.len() as i64);
        self.solver.assert(&c.ge(&zero));
        self.solver.assert(&c.lt(&n));
        self.consts.borrow_mut().push(c);
        self.index.borrow_mut().insert(setting.key.clone(), id);
        self.settings.borrow_mut().push(setting);
        Ok(id)
    }

    pub fn and(&self, parts: Vec<Cond>) -> Cond {
        let mut flat: Vec<Cond> = Vec::new();
        for p in parts {
            match p {
                Cond::True => (),
                Cond::False => return Cond::False,
                Cond::And(v) => flat.extend(v.iter().cloned()),
                other => flat.push(other),
            }
        }
        flat.sort();
        flat.dedup();
        for w in flat.windows(2) {
            if let (Cond::Is(a, x), Cond::Is(b, y)) = (&w[0], &w[1]) {
                if a == b && x != y {
                    return Cond::False;
                }
            }
        }
        match flat.len() {
            0 => Cond::True,
            1 => flat.pop().unwrap(),
            _ => Cond::And(flat),
        }
    }

    pub fn or(&self, parts: Vec<Cond>) -> Cond {
        let mut flat: Vec<Cond> = Vec::new();
        for p in parts {
            match p {
                Cond::False => (),
                Cond::True => return Cond::True,
                Cond::Or(v) => flat.extend(v.iter().cloned()),
                other => flat.push(other),
            }
        }
        flat.sort();
        flat.dedup();
        let mut by_setting: HashMap<SettingId, Vec<u32>> = HashMap::new();
        let mut only_literals = true;
        for c in &flat {
            match c {
                Cond::Is(s, j) => by_setting.entry(*s).or_default().push(*j),
                _ => only_literals = false,
            }
        }
        if only_literals {
            for (s, mut js) in by_setting {
                js.sort();
                js.dedup();
                if js.len() == self.n_choices(s) {
                    return Cond::True;
                }
            }
        }
        match flat.len() {
            0 => Cond::False,
            1 => flat.pop().unwrap(),
            _ => Cond::Or(flat),
        }
    }

    pub fn not(&self, cond: &Cond) -> Cond {
        match cond {
            Cond::True => Cond::False,
            Cond::False => Cond::True,
            Cond::Is(s, j) => {
                let n = self.n_choices(*s) as u32;
                self.or((0..n).filter(|k| k != j).map(|k| Cond::Is(*s, k)).collect())
            }
            Cond::And(v) => self.or(v.iter().map(|x| self.not(x)).collect()),
            Cond::Or(v) => self.and(v.iter().map(|x| self.not(x)).collect()),
        }
    }

    pub fn assume(&self, c: &Cond) {
        let b = self.to_z3(c);
        self.solver.assert(&b);
        //self.sat_cache.borrow_mut().clear();
    }

    pub fn sat(&self, c: &Cond) -> bool {
        match c {
            Cond::True => return true,
            Cond::False => return false,
            _ => (),
        }
        //if let Some(v) = self.sat_cache.borrow().get(c) {
        //    return *v;
        //}
        let b = self.to_z3(c);
        let r = matches!(self.solver.check_assumptions(&[b]), SatResult::Sat);
        //self.sat
        r
    }

    fn to_z3(&self, c: &Cond) -> Bool {
        match c {
            Cond::True => Bool::from_bool(true),
            Cond::False => Bool::from_bool(false),
            Cond::Is(s, j) => {
                let k = self.consts.borrow()[*s as usize].clone();
                k.eq(Int::from_i64(*j as i64))
            }
            Cond::And(v) => {
                let parts: Vec<Bool> = v.iter().map(|x| self.to_z3(x)).collect();
                let refs: Vec<&Bool> = parts.iter().collect();
                Bool::and(&refs)
            }
            Cond::Or(v) => {
                let parts: Vec<Bool> = v.iter().map(|x| self.to_z3(x)).collect();
                let refs: Vec<&Bool> = parts.iter().collect();
                Bool::or(&refs)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Cond {
    True,
    False,
    Is(SettingId, u32),
    And(Vec<Cond>),
    Or(Vec<Cond>),
}
