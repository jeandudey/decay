use {
    eyre::bail,
    std::{
        cell::RefCell,
        collections::HashMap, //
    },
    z3::{
        Context,
        Solver,
        ast::Int, //
    },
};

pub type SettingId = u32;

#[derive(Debug)]
pub struct Setting {
    pub key: String,
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

    pub fn get(&self, key: &str) -> Option<SettingId> {
        self.index.borrow().get(key).copied()
    }

    pub fn intern(&self, setting: Setting) -> eyre::Result<SettingId> {
        if let Some(id) = self.get(&setting.key) {
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
}
