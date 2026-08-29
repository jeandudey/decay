use {
    crate::ast::{
        Args, BinOp, BinOpKind, Block, Call, Expr, IfStmt, Method, ProjectOptionKind,
        ProjectOptions, Stmt, UnOpKind,
        interp::Flow::Normal,
        sym::{
            Cond,
            Env,
            Setting,
            SettingId, //
        },
    },
    eyre::{
        Ok,
        OptionExt,
        bail, //
    },
    std::{
        collections::{
            BTreeMap,
            HashMap, //
        },
        mem,
        path::PathBuf,
        rc::Rc,
    },
};

#[derive(Debug)]
pub struct Interp<'a> {
    options: Option<&'a ProjectOptions>,
    systems: &'a HashMap<String, String>,
    env: Env,
    project: Project,
    vars: HashMap<String, Val>,
    pc: Cond,
    flow: Flow,
}

impl<'a> Interp<'a> {
    pub fn new(options: Option<&'a ProjectOptions>, systems: &'a HashMap<String, String>) -> Self {
        let mut vars = HashMap::new();
        vars.insert("meson".into(), Val::Obj(Rc::new(Obj::Meson)));
        vars.insert(
            "host_machine".into(),
            Val::Obj(Rc::new(Obj::Machine(MachineKind::Host))),
        );

        Self {
            options,
            systems,
            env: Env::new(),
            project: Project {
                name: String::new(),
                languages: Vec::new(),
                version: None,
                default_options: None,
                license: None,
            },
            vars,
            pc: Cond::True,
            flow: Normal,
        }
    }

    pub fn exec_block(&mut self, block: &Block) -> eyre::Result<()> {
        for stmt in &block.0 {
            self.exec_stmt(stmt)?;
            if self.flow != Flow::Normal {
                break;
            }
        }
        Ok(())
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> eyre::Result<()> {
        match stmt {
            Stmt::Expr(expr) => {
                self.eval(expr)?;
            }
            Stmt::Assign(assign) => {
                let value = self.eval(&assign.value)?;
                let value = if assign.is_plus {
                    let old = self
                        .vars
                        .get(&assign.name)
                        .cloned()
                        .ok_or_eyre("+= on an undefined variable")?;
                    self.add(&old, &value)?
                } else {
                    value
                };
                self.vars.insert(assign.name.clone(), value);
            }
            Stmt::If(stmt) => self.exec_if(stmt)?,
            _ => todo!("{stmt:?}"),
        }

        Ok(())
    }

    fn exec_if(&mut self, stmt: &IfStmt) -> eyre::Result<()> {
        let entry_pc = self.pc.clone();

        let mut branches: Vec<(Cond, HashMap<String, Val>)> = Vec::new();
        let mut not_taken = Cond::True;

        for (condition, block) in &stmt.arms {
            let condition = self.eval(&condition)?;
            let c = condition.truth(&self.env)?;
            let g = self
                .env
                .and(vec![entry_pc.clone(), not_taken.clone(), c.clone()]);
            not_taken = self.env.and(vec![not_taken, self.env.not(&c)]);

            if !self.env.sat(&g) {
                println!("not sat");
                continue;
            } else {
                println!("sat");
            }

            if let Some(st) = self.run_branch(g.clone(), block)? {
                branches.push((g, st));
            }
            if self.flow != Flow::Normal && self.flow != Flow::Abort {
                break;
            }
        }

        if let Some(elseblock) = &stmt.elseblock {
            self.exec_block(&elseblock)?;
        }

        Ok(())
    }

    fn run_branch(&mut self, g: Cond, b: &Block) -> eyre::Result<Option<HashMap<String, Val>>> {
        let saved_vars = self.vars.clone();
        let saved_pc = mem::replace(&mut self.pc, g);
        let saved_flow = self.flow;
        let r = self.exec_block(b);
        let out = mem::replace(&mut self.vars, saved_vars);
        self.pc = saved_pc;
        let branch_flow = self.flow;
        self.flow = saved_flow;
        r?;
        if branch_flow == Flow::Abort {
            return Ok(None);
        }
        if branch_flow != Flow::Normal {
            self.flow = branch_flow;
        }
        Ok(Some(out))
    }

    fn add(&mut self, a: &Val, b: &Val) -> eyre::Result<Val> {
        if a.is_arrayish() || b.is_arrayish() {
            return Ok(concat(vec![a.lift_array(), b.lift_array()]));
        }

        map_guarded2(&self.env, a, b, &self.pc, &mut |a, b, _| {
            binop_concrete(&self.env, BinOpKind::Add, a, b)
        })
    }

    fn eval(&mut self, expr: &Expr) -> eyre::Result<Val> {
        match expr {
            Expr::Bool(b) => Ok(Val::Bool(*b)),
            Expr::String(s) => Ok(Val::String(s.clone())),
            Expr::Call(call) => self.call(call),
            Expr::Method(method) => self.method(method),
            Expr::Id(id) => match self.vars.get(id) {
                Some(Val::Unset) | None => bail!("Undefined variable `{id}`"),
                Some(v) => Ok(v.clone()),
            },
            Expr::Number(v) => Ok(Val::Int(*v)),
            Expr::Array(array) => Ok(Val::Array(
                array
                    .iter()
                    .map(|v| self.eval(v))
                    .collect::<eyre::Result<_>>()?,
            )),
            Expr::Index(index) => {
                let obj = self.eval(&index.obj)?;
                let idx = self.eval(&index.index)?;
                self.index(obj, idx)
            }
            Expr::UnOp(unop) => {
                let v = self.eval(&unop.val)?;
                match unop.kind {
                    UnOpKind::Not => {
                        let c = v.truth(&self.env)?;
                        Ok(cond_val(&self.env, self.env.not(&c)))
                    }
                }
            }
            Expr::BinOp(binop) => self.eval_binop(&binop),
            _ => bail!("{expr:?}"),
        }
    }

    fn call(&mut self, call: &Call) -> eyre::Result<Val> {
        let (positional, keyword) = self.eval_args(&call.args)?;

        match call.name.as_str() {
            "project" => {
                self.project.name = positional
                    .first()
                    .ok_or_eyre("Expected project name")?
                    .as_str()
                    .ok_or_eyre("Project name should be a string")?
                    .to_owned();

                self.project.languages = positional
                    .iter()
                    .skip(1)
                    .map(|v| {
                        v.as_str()
                            .ok_or_eyre("Language should be a string")
                            .map(|v| v.to_owned())
                    })
                    .collect::<eyre::Result<_>>()?;

                self.project.version = keyword
                    .get("version")
                    .map(|v| {
                        v.as_str()
                            .map(|v| v.to_owned())
                            .ok_or_eyre("version should be a string")
                    })
                    .transpose()?;

                self.project.default_options = keyword
                    .get("default_options")
                    .map(|v| {
                        v.as_array()
                            .map(|v| {
                                v.iter()
                                    .map(|v| {
                                        v.as_str()
                                            .map(|v| v.to_owned())
                                            .ok_or_eyre("default_option values should be a string")
                                    })
                                    .collect::<eyre::Result<Vec<_>>>()
                            })
                            .ok_or_eyre("default_options should be an array")
                            .flatten()
                    })
                    .transpose()?;

                self.project.license = keyword
                    .get("license")
                    .map(|v| {
                        v.as_str()
                            .map(|v| v.to_owned())
                            .ok_or_eyre("license should be a string")
                    })
                    .transpose()?;

                Ok(Val::Unset)
            }
            "get_option" => {
                let name = positional
                    .first()
                    .ok_or_eyre("Expected option name")?
                    .as_str()
                    .ok_or_eyre("Option name should be a string")?;

                match name {
                    "prefix" => Ok(Val::String("/usr".into())),
                    "libdir" => Ok(Val::String("lib".into())),
                    "libexecdir" => Ok(Val::String("libexec".into())),
                    "datadir" => Ok(Val::String("share".into())),
                    "includedir" => Ok(Val::String("include".into())),
                    "default_library" => {
                        let id = self.env.intern(Setting {
                            key: "builtin:default_library".into(),
                            truthy: None,
                            choices: vec!["shared".into(), "static".into(), "both".into()],
                        })?;
                        Ok(Val::Sym(id))
                    }
                    name if let Some(options) = self.options => {
                        if let Some(option) = options.get(name) {
                            match &option.kind {
                                ProjectOptionKind::Bool { .. } => {
                                    return self.intern_bool(format!("opt:{name}")).map(Val::Sym);
                                }
                                ProjectOptionKind::Combo { choices, .. } => {
                                    let id = self.env.intern(Setting {
                                        key: format!("opt:{name}"),
                                        truthy: None,
                                        choices: choices.clone(),
                                    })?;
                                    return Ok(Val::Sym(id));
                                }
                            }
                        }
                        bail!("Unknown option {name}")
                    }
                    _ => bail!("Unknown option {name}"),
                }
            }
            "join_paths" => {
                let segments = positional
                    .iter()
                    .map(|v| v.as_str().ok_or_eyre("path segments should strings"))
                    .collect::<eyre::Result<Vec<_>>>()?;

                let path = segments
                    .iter()
                    .fold(PathBuf::new(), |path, segment| path.join(segment));

                Ok(Val::String(
                    path.to_str()
                        .ok_or_eyre("Failed to convert path to a string")?
                        .to_owned(),
                ))
            }
            "configuration_data" => {
                if !positional.is_empty() {
                    bail!("configuration data arguments not yet implemented");
                }

                Ok(Val::Obj(Rc::new(Obj::CfgData(HashMap::new()))))
            }
            "error" => {
                let text = positional
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .ok_or_eyre("Error text must be a string")
                            .map(|v| v.to_owned())
                    })
                    .collect::<eyre::Result<Vec<_>>>()?
                    .join(" ");
                println!("{text}");
                let neg = self.env.not(&self.pc);
                self.env.assume(&neg);
                self.flow = Flow::Abort;
                Ok(Val::Unset)
            }
            _ => bail!(
                "Unknown function call {} args {positional:?} {keyword:?}",
                call.name
            ),
        }
    }

    fn method(&mut self, method: &Method) -> eyre::Result<Val> {
        let obj = self.eval(&method.obj)?;
        let (positional, keyword) = self.eval_args(&method.args)?;

        if let Val::Obj(obj) = &obj {
            match (&**obj, method.name.as_str()) {
                (Obj::Meson, "project_version") => {
                    return Ok(Val::String(
                        self.project
                            .version
                            .as_ref()
                            .ok_or_eyre("No project version")?
                            .clone(),
                    ));
                }
                (Obj::Meson, "project_name") => {
                    return Ok(Val::String(self.project.name.clone()));
                }
                (Obj::Meson, "get_compiler") => {
                    let compiler = positional
                        .first()
                        .map(|v| {
                            v.as_str()
                                .ok_or_eyre("compiler argument should be a string")
                        })
                        .transpose()?
                        .ok_or_eyre("expected compiler argument")?;

                    match compiler {
                        "c" => return Ok(Val::Obj(Rc::new(Obj::Compiler(Lang::C)))),
                        _ => bail!("Unknow compiler for get_compiler {compiler}"),
                    }
                }
                (Obj::Machine(_), "system") => {
                    let id = self.env.intern(Setting {
                        key: "machine:host_system".into(),
                        truthy: None,
                        choices: self.systems.keys().cloned().collect(),
                    })?;
                    return Ok(Val::Sym(id));
                }
                (Obj::CfgData(data), "set_quoted") => {
                    eprintln!("todo set_quoted");
                    return Ok(Val::Unset);
                }
                (Obj::CfgData(data), "set") => {
                    eprintln!("todo set");
                    return Ok(Val::Unset);
                }
                (Obj::CfgData(data), "set10") => {
                    eprintln!("todo set10");
                    return Ok(Val::Unset);
                }
                (Obj::Compiler(lang), "has_header") => {
                    let header = positional
                        .first()
                        .map(|v| v.as_str().ok_or_eyre("Header should be a string"))
                        .transpose()?
                        .ok_or_eyre("Expected header")?;
                    let id = self.intern_bool(format!(
                        "probe:{}:has_header:{header}",
                        match lang {
                            Lang::C => "c",
                        }
                    ))?;
                    return Ok(Val::Sym(id));
                }
                (Obj::Compiler(lang), "get_id") => {
                    // TODO.
                    return Ok(Val::String("gnu".into()));
                }
                (Obj::Compiler(lang), "get_supported_arguments") => {
                    // TODO.
                    return Ok(Val::Array(positional));
                }
                (Obj::Compiler(lang), "find_library") => {
                    let name = positional
                        .first()
                        .map(|v| v.as_str().ok_or_eyre("Name should be a string"))
                        .transpose()?
                        .ok_or_eyre("Expected name")?;

                    return self
                        .intern_bool(format!(
                            "lib:{}:{name}",
                            match lang {
                                Lang::C => "c",
                            }
                        ))
                        .map(Val::Sym);
                }
                (obj, name) => {
                    bail!("Unknown method `{name}` for obj {obj:?} args {positional:?} {keyword:?}")
                }
            }
        }

        match (obj, method.name.as_str()) {
            (Val::String(s), "format") => {
                eprintln!("todo format");
                Ok(Val::String(s.clone()))
            }
            (Val::String(s), "split") => {
                let pat = positional
                    .first()
                    .map(|v| {
                        v.as_str()
                            .map(|v| v.to_owned())
                            .ok_or_eyre("split expects a string argument")
                    })
                    .transpose()?
                    .ok_or_eyre("split expects a string argument")?;

                Ok(Val::Array(
                    s.split(&pat).map(|v| Val::String(v.to_owned())).collect(),
                ))
            }
            (Val::String(s), "to_int") => Ok(i64::from_str_radix(&s, 10).map(Val::Int)?),
            (Val::Array(items), "contains") => {
                let pat = positional
                    .first()
                    .ok_or_eyre("contains expects a string argument")?;
                Ok(Val::Bool(items.iter().any(|x| x == pat)))
            }
            (obj, name) => {
                bail!("Unknown method {name} on {obj:?} args {positional:?} {keyword:?}")
            }
        }
    }

    fn index(&mut self, obj: Val, idx: Val) -> eyre::Result<Val> {
        match (obj, idx) {
            (Val::Array(v), Val::Int(idx)) => v
                .get(usize::try_from(idx)?)
                .ok_or_eyre("index out of bounds")
                .cloned(),
            (obj, idx) => bail!("Unknow indexing method: {obj:?} {idx:?}"),
        }
    }

    fn eval_binop(&mut self, binop: &BinOp) -> eyre::Result<Val> {
        if matches!(binop.kind, BinOpKind::Or) {
            let l = self.eval(&binop.lhs)?;
            let lc = l.truth(&self.env)?;
            let r = self.eval(&binop.rhs)?;
            let rc = r.truth(&self.env)?;
            let c = match binop.kind {
                _ => self.env.or(vec![lc, rc]),
            };
            return Ok(cond_val(&self.env, c));
        }

        let l = self.eval(&binop.lhs)?;
        let r = self.eval(&binop.rhs)?;
        let pc = self.pc.clone();
        let kind = binop.kind;
        let env = &self.env;
        map_guarded2(env, &l, &r, &pc, &mut |a, bb, _| {
            binop_concrete(env, kind, a, bb)
        })
    }

    fn eval_args(&mut self, args: &Args) -> eyre::Result<(Vec<Val>, BTreeMap<String, Val>)> {
        let positional = args
            .positional
            .iter()
            .map(|arg| self.eval(arg))
            .collect::<eyre::Result<Vec<_>>>()?;

        let mut keyword = BTreeMap::new();
        for key in &args.order {
            let value = args
                .kwargs
                .get(key)
                .ok_or_eyre("order and kwargs should have the same keys")?;
            keyword.insert(key.clone(), self.eval(value)?);
        }
        Ok((positional, keyword))
    }

    fn intern_bool(&mut self, key: String) -> eyre::Result<SettingId> {
        let id = self.env.intern(Setting {
            key,
            truthy: Some(0),
            choices: vec!["true".into(), "false".into()],
        })?;
        Ok(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Normal,
    Break,
    Continue,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    Bool(bool),
    String(String),
    Obj(Rc<Obj>),
    Array(Vec<Val>),
    Int(i64),
    Sym(SettingId),
    Guarded(Vec<(Cond, Val)>),
    Concat(Vec<Val>),
    Unset,
}

impl Val {
    fn lift_array(&self) -> Val {
        match self {
            Val::Array(_) | Val::Concat(_) => self.clone(),
            Val::Guarded(arms) if arms.iter().all(|(_, x)| x.is_arrayish()) => self.clone(),
            _ => self.lift_element(),
        }
    }

    fn lift_element(&self) -> Val {
        match self {
            Val::Array(_) | Val::Concat(_) => self.clone(),
            Val::Guarded(arms) => {
                let mapped: Vec<Val> = arms.iter().map(|(_, x)| x.clone()).collect();
                if mapped.iter().all(|x| x.is_arrayish()) {
                    Val::Guarded(arms.clone())
                } else {
                    Val::Guarded(
                        arms.iter()
                            .map(|(g, x)| (g.clone(), x.lift_element()))
                            .collect(),
                    )
                }
            }
            Val::Unset => panic!("unset in lift element"),
            _ => Val::Array(vec![self.clone()]),
        }
    }

    fn is_arrayish(&self) -> bool {
        matches!(self, Val::Array(_) | Val::Concat(_))
    }

    fn truth(&self, env: &Env) -> eyre::Result<Cond> {
        match self {
            Val::Bool(true) => Ok(Cond::True),
            Val::Bool(false) => Ok(Cond::False),
            Val::Sym(id) => {
                let setting = env.setting(*id)?;
                match setting.truthy {
                    Some(i) => Ok(Cond::Is(*id, i as u32)),
                    None => bail!("option '{}' is not a boolean", setting.key),
                }
            }
            Val::Guarded(arms) => {
                let mut parts = Vec::new();
                for (g, x) in arms.iter() {
                    let t = x.truth(env)?;
                    parts.push(env.and(vec![g.clone(), t]));
                }
                Ok(env.or(parts))
            }
            Val::Unset => bail!("condition reads a variable that is unset on this path"),
            other => bail!("expected bool in condition, got {other:?}"),
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Val::String(v) => Some(v),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Val]> {
        match self {
            Val::Array(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Obj {
    Compiler(Lang),
    Machine(MachineKind),
    Meson,
    CfgData(HashMap<String, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Lang {
    C,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineKind {
    Host,
}

#[derive(Debug)]
struct Project {
    name: String,
    languages: Vec<String>,
    version: Option<String>,
    default_options: Option<Vec<String>>,
    license: Option<String>,
}

fn binop_concrete(env: &Env, kind: BinOpKind, a: &Val, b: &Val) -> eyre::Result<Val> {
    match kind {
        BinOpKind::Eq | BinOpKind::Ne => {
            let c = eq_cond(env, a, b)?;
            let c = if kind == BinOpKind::Ne {
                env.not(&c)
            } else {
                c
            };
            Ok(cond_val(env, c))
        }
        _ => todo!("{kind:?}"),
    }
}

fn eq_cond(env: &Env, a: &Val, b: &Val) -> eyre::Result<Cond> {
    match (a, b) {
        (Val::Sym(s), other) | (other, Val::Sym(s)) => {
            let want = match other {
                Val::String(x) => x.to_string(),
                Val::Bool(x) => x.to_string(),
                Val::Int(x) => x.to_string(),
                Val::Sym(t) if t == s => return Ok(Cond::True),
                o => bail!("cannot compare against {o:?}"),
            };

            Ok(match env.choice_index(*s, &want) {
                Some(i) => Cond::Is(*s, i),
                None => Cond::False,
            })
        }
        (x, y) => {
            if x == y {
                Ok(Cond::True)
            } else {
                Ok(Cond::False)
            }
        }
    }
}

fn map_guarded2<F>(env: &Env, a: &Val, b: &Val, ctx: &Cond, f: &mut F) -> eyre::Result<Val>
where
    F: FnMut(&Val, &Val, &Cond) -> eyre::Result<Val>,
{
    match (a, b) {
        (Val::Guarded(arms), _) => {
            let mut out = Vec::new();
            for (g, x) in arms.iter() {
                let c = env.and(vec![ctx.clone(), g.clone()]);
                if !env.sat(&c) {
                    continue;
                }
                out.push((g.clone(), map_guarded2(env, x, b, &c, f)?));
            }
            Ok(guarded(env, out))
        }
        (_, Val::Guarded(arms)) => {
            let mut out = Vec::new();
            for (g, y) in arms.iter() {
                let c = env.and(vec![ctx.clone(), g.clone()]);
                if !env.sat(&c) {
                    continue;
                }
                out.push((g.clone(), map_guarded2(env, a, y, &c, f)?));
            }
            Ok(guarded(env, out))
        }
        _ => f(a, b, ctx),
    }
}

fn guarded(env: &Env, arms: Vec<(Cond, Val)>) -> Val {
    let v = guarded_flat(env, arms);
    match &v {
        Val::Guarded(a) => todo!(),
        _ => v,
    }
}

fn guarded_flat(env: &Env, arms: Vec<(Cond, Val)>) -> Val {
    let mut flat: Vec<(Cond, Val)> = Vec::new();
    let push = |g: Cond, v: Val, out: &mut Vec<(Cond, Val)>| {
        if matches!(g, Cond::False) {
            return;
        }
        out.push((g, v));
    };
    for (g, v) in arms {
        if matches!(g, Cond::False) || !env.sat(&g) {
            continue;
        }
        match v {
            Val::Guarded(inner) => {
                for (h, iv) in inner.iter() {
                    let c = env.and(vec![g.clone(), h.clone()]);
                    if env.sat(&c) {
                        push(c, iv.clone(), &mut flat);
                    }
                }
            }
            other => push(g, other, &mut flat),
        }
    }
    if flat.is_empty() {
        return Val::Unset;
    }
    let mut grouped: Vec<(Cond, Val)> = Vec::new();
    for (g, v) in flat {
        if let Some(slot) = grouped.iter_mut().find(|(_, w)| *w == v) {
            slot.0 = env.or(vec![slot.0.clone(), g]);
        } else {
            grouped.push((g, v));
        }
    }
    if grouped.len() == 1 {
        return grouped.pop().unwrap().1;
    }
    grouped.sort_by(|a, b| a.0.cmp(&b.0));
    Val::Guarded(grouped)
}

fn cond_val(env: &Env, c: Cond) -> Val {
    match c {
        Cond::True => Val::Bool(true),
        Cond::False => Val::Bool(false),
        c => {
            let n = env.not(&c);
            guarded_flat(env, vec![(c, Val::Bool(true)), (n, Val::Bool(false))])
        }
    }
}

/// Normalize a concatenation: flatten nesting, drop empties, fuse adjacent
/// literal arrays, collapse singletons.
fn concat(parts: Vec<Val>) -> Val {
    let mut flat: Vec<Val> = Vec::new();
    for p in parts {
        match p {
            Val::Concat(v) => flat.extend(v.iter().cloned()),
            Val::Array(v) if v.is_empty() => {}
            Val::Unset => {}
            other => flat.push(other),
        }
    }
    let mut fused: Vec<Val> = Vec::new();
    for p in flat {
        match (fused.last_mut(), &p) {
            (Some(Val::Array(a)), Val::Array(b)) => {
                let mut n = a.clone();
                n.extend(b.iter().cloned());
                *fused.last_mut().unwrap() = Val::Array(n);
            }
            _ => fused.push(p),
        }
    }
    match fused.len() {
        0 => Val::Array(Vec::new()),
        1 => fused.pop().unwrap(),
        _ => Val::Concat(fused),
    }
}
