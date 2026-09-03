use {
    crate::{
        obj::{
            Lang,
            Machine,
            Obj, //
        },
        oracle::Oracle,
        project::Project,
        val::{
            Value,
            Variant, //
            Variational,
        },
    },
    decay_meson_ast::{
        Args,
        Block,
        Call,
        Expr,
        Index,
        Method,
        Stmt, //
    },
    decay_meson_logic::{
        Logic,
        Pc,
        Solver,
        Z3Solver, //
    },
    eyre::{
        Context,
        Ok,
        OptionExt,
        bail, //
    },
    std::{
        collections::HashMap,
        mem,
        rc::Rc,
        str::FromStr, //
    },
};

pub mod obj;
pub mod oracle;
pub mod val;

mod project;

//type Builtin<'a, S, O> = fn(&mut Interpreter<'a, S, O>, &ConcreteArgs) -> eyre::Result<VarVal>;

pub fn eval<O: Oracle>(oracle: &O, block: &Block) -> eyre::Result<()> {
    let solver = Z3Solver {};
    let mut interp = Interpreter::new(solver, oracle);
    interp.block(block)?;
    Ok(())
}

#[derive(Debug)]
pub struct Interpreter<'a, S, O>
where
    S: Solver,
    O: Oracle,
{
    /// The logic backend.
    logic: Logic<S>,
    /// Oracle providing choices of values or concrete answers.
    oracle: &'a O,
    /// The current presence condition.
    pc: Pc,
    builtin_objects: HashMap<&'static str, Obj>,
    store: HashMap<String, Variational<Value>>,
    project: Option<Project>,
}

impl<'a, S, O> Interpreter<'a, S, O>
where
    S: Solver,
    O: Oracle,
{
    pub fn new(solver: S, oracle: &'a O) -> Self {
        let mut builtin_objects = HashMap::new();
        builtin_objects.insert("meson", Obj::Meson);
        builtin_objects.insert("host_machine", Obj::Machine(Machine::Host));

        Self {
            logic: Logic::new(solver),
            oracle,
            pc: Pc::TRUE,
            builtin_objects,
            store: HashMap::new(),
            project: None,
        }
    }

    fn block(&mut self, block: &Block) -> eyre::Result<()> {
        for stmt in &block.0 {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> eyre::Result<()> {
        match stmt {
            Stmt::Expr(v) => {
                self.expr(v)?;
            }
            _ => bail!("unimplmented {stmt:?}"),
            //Stmt::Bind(BindStmt { name, rvalue }) => {
            //    let val = self.rvalue(rvalue)?;
            //    bail!("assign");
            //    //self.env.insert(name.clone(), val);
            //}
            //Stmt::If(v) => bail!("unimplemented {v:?}"),
        }
        Ok(())
    }

    fn expr(&mut self, expr: &Expr) -> eyre::Result<Variational<Value>> {
        Ok(match expr {
            Expr::List(v) => {
                let mut elements = Vec::with_capacity(v.len());
                for element in v {
                    let mut v = self.expr(element)?;
                    v.normalize(&mut self.logic);
                    elements.extend(v.into_variants());
                }
                Variant::new(self.pc, Value::List(Rc::new(elements))).into()
            }
            Expr::Call(Call { name, args }) => {
                let args = self.args(args)?;
                let mut out: Variational<Value> = Variational::empty();
                for (pc, args) in &args {
                    let v = self.with_pc(*pc, |this| this.call(name, args))?;
                    out.extend(v.into_variants());
                }
                out
            }
            Expr::FormatString(v) | Expr::String(v) => Variant::new(self.pc, v.into()).into(),
            _ => bail!("unimplemented {expr:?}"),
        })
    }

    fn call(&mut self, name: &str, args: &ArgSet) -> eyre::Result<Variational<Value>> {
        Ok(match name {
            "project" => self.call_project(&args)?,
            _ => bail!("unimplemented {name:?} {args:?}"),
        })
    }

    fn args(&mut self, args: &Args) -> eyre::Result<Vec<(Pc, ArgSet)>> {
        let pos = args
            .pos
            .iter()
            .map(|expr| self.expr(expr))
            .collect::<eyre::Result<Vec<_>>>()?;
        let kw: Vec<(Rc<str>, Variational<Value>)> = args
            .order
            .iter()
            .map(|name| {
                let expr = args.kw.get(name).unwrap();
                Ok((Rc::from(name.as_str()), self.expr(expr)?))
            })
            .collect::<eyre::Result<Vec<_>>>()?;

        let mut out = vec![(
            self.pc,
            ArgSet {
                pos: Vec::new(),
                kw: Vec::new(),
            },
        )];

        for g in &pos {
            let mut next = Vec::new();
            for (apc, set) in &out {
                for var in g.variants() {
                    let p = self.logic.and(*apc, var.cond);
                    if p.is_false() {
                        continue;
                    }
                    let mut s = set.clone();
                    s.pos.push(var.value.clone());
                    next.push((p, s));
                }
            }
            out = next;
        }

        for (name, g) in &kw {
            let mut next = Vec::new();
            for (apc, set) in &out {
                for var in g.variants() {
                    let p = self.logic.and(*apc, var.cond);
                    if p.is_false() {
                        continue;
                    }
                    let mut s = set.clone();
                    s.kw.push((name.clone(), var.value.clone()));
                    next.push((p, s));
                }
            }
            out = next;
        }
        Ok(out)
    }

    fn with_pc<R>(&mut self, pc: Pc, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved_pc = mem::replace(&mut self.pc, pc);
        let r = f(self);
        self.pc = saved_pc;
        r
    }

    fn call_project(&mut self, args: &ArgSet) -> eyre::Result<Variational<Value>> {
        if self.project.is_some() {
            bail!("project() called twice");
        }

        if args.pos.is_empty() {
            bail!("project() requires a name");
        }

        let Some(name) = self.as_str(&args.pos[0]) else {
            bail!("expected name");
        };

        let mut langs = Vec::new();
        for v in &args.pos[1..] {
            self.collect_strs(self.pc, v, &mut langs);
        }

        bail!("unimplemented project {name} {langs:?} {args:?}")
    }

    fn as_str(&mut self, v: &Value) -> Option<Rc<str>> {
        match v {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn collect_strs(&mut self, pc: Pc, v: &Value, out: &mut Vec<Variant<Rc<str>>>) {
        match v {
            Value::Str(s) => out.push(Variant::new(pc, s.clone())),
            Value::List(items) => {
                for it in items.iter() {
                    let p = self.logic.and(pc, it.cond);
                    if p.is_false() {
                        continue;
                    }
                    self.collect_strs(p, &it.value, out);
                }
            }
            _ => todo!(),
        }
    }

    /*
    fn atom(&self, atom: &Atom) -> eyre::Result<Val> {
        Ok(match atom {
            //Atom::Var(v) => {
            //    if let Some(v) = self.env.get(v.as_str()) {
            //        return Ok(v.clone());
            //    }

            //    if let Some(obj) = self.builtin_objects.get(v.as_str()) {
            //        return Ok(Val::Pure(Const::Obj(obj.clone())));
            //    }

            //    bail!("unknown variable {v}")
            //}
            Atom::String(v) => Val::from_string(self.pc, v.clone()),
            //Atom::Int(v) => Val::Pure(Const::Int(*v)),
            //Atom::Bool(v) => Val::Pure(Const::Bool(*v)),
            _ => bail!("unimplemented {atom:?}"),
        })
    }

    fn rvalue(&mut self, rvalue: &RValue) -> eyre::Result<Val> {
        Ok(match rvalue {
            //RValue::Pure(v) => self.atom(v)?,
            RValue::Array(v) => {
                bail!("unimplemented {v:?}")
                //let elements = v
                //    .iter()
                //    .map(|v| self.atom(v))
                //    .collect::<eyre::Result<_>>()?;
                //array_lit(self.guard_ctx, self.pc, elements)
            }
            //RValue::Call(Call { name, args }) => {
            //    let (pos, kw) = self.args(args)?;
            //    self.call(name, pos, kw)?
            //}
            //RValue::Method(Method { obj, name, args }) => {
            //    let obj = self.atom(&obj)?;
            //    let (pos, kw) = self.args(args)?;
            //    self.method(obj, name, pos, kw)?
            //}
            //RValue::Index(Index { obj, index }) => {
            //    let obj = self.atom(&obj)?;
            //    let index = self.atom(&index)?;
            //    index_const(
            //        obj.expect_pure().wrap_err("add support for symbolic obj")?,
            //        index
            //            .expect_pure()
            //            .wrap_err("add support for symbolic index")?,
            //    )
            //    .map(Val::Pure)?
            //}
            /*
            RValue::Index(v) => Const::Unset,
            RValue::BinOp(v) => Const::Unset,
            */
            _ => bail!("unimplemented {rvalue:?}"),
        })
    }
    */

    /*


    fn call(
        &mut self,
        name: &str,
        pos: Vec<Val<G::Id>>,
        kw: Vec<(String, Val<G::Id>)>,
    ) -> eyre::Result<Val<G::Id>> {
        match name {
            "project" => {
                let name = pos
                    .get(0)
                    .ok_or_eyre("expected project name")?
                    .expect_pure()?
                    .expect_string()?;
                let version = kw
                    .iter()
                    .find(|(k, _)| k == "version")
                    .map(|(_, v)| v.expect_pure()?.expect_string())
                    .transpose()?;
                self.call_project(name, version)
            }
            "get_option" => {
                let name = pos
                    .first()
                    .ok_or_eyre("expected dependency name")?
                    .expect_pure()?
                    .expect_string()?;

                let v = self.oracle.get_option(&name);
                if matches!(v, Const::Unset) {
                    bail!("option {name} returned unset");
                }

                Ok(Val::Pure(v))
            }
            "join_paths" => {
                let segments = pos
                    .iter()
                    .map(|v| v.expect_pure()?.expect_string())
                    .collect::<eyre::Result<Vec<_>>>()?;
                Ok(Val::Pure(Const::String(segments.join("/"))))
            }
            "configuration_data" => Ok(Val::Pure(Const::Unset)),
            _ => bail!("unimplemented call {name} {pos:?} {kw:?}"),
        }
    }

    fn method(
        &mut self,
        obj: Val<G::Id>,
        name: &str,
        pos: Vec<Val<G::Id>>,
        kw: Vec<(String, Val<G::Id>)>,
    ) -> eyre::Result<Val<G::Id>> {
        let obj = obj
            .expect_pure()
            .wrap_err("if you get this error probably revisit this code as it might be wrong")?;

        match (obj, name) {
            (Const::Obj(Obj::Meson), "project_name") => {
                let project = self
                    .project
                    .as_ref()
                    .ok_or_eyre("project() has not been called")?;
                Ok(Val::Pure(Const::String(project.name.clone())))
            }
            (Const::Obj(Obj::Meson), "project_version") => {
                let project = self
                    .project
                    .as_ref()
                    .ok_or_eyre("project() has not been called")?;
                let version = project
                    .version
                    .as_ref()
                    .ok_or_eyre("version not specified in project() call")?;
                Ok(Val::Pure(Const::String(version.clone())))
            }
            (Const::Obj(Obj::Meson), "get_compiler") => {
                let compiler = pos
                    .first()
                    .ok_or_eyre("expected compiler")?
                    .expect_pure()?
                    .expect_string()?;

                let lang = Lang::from_str(&compiler)?;
                Ok(Val::Pure(Const::Obj(Obj::Compiler(lang))))
            }
            (Const::Obj(Obj::Machine(machine)), "system") => {
                Ok(Val::Pure(self.oracle.machine_system(*machine)))
            }
            (Const::String(v), "split") => {
                let pat = pos
                    .first()
                    .ok_or_eyre("expected pattern")?
                    .expect_pure()?
                    .expect_string()?;
                Ok(array_lit(
                    self.guard_ctx,
                    self.pc,
                    v.split(pat.as_str())
                        .map(|v| Const::String(v.into()))
                        .map(Val::Pure)
                        .collect(),
                ))
            }
            (Const::String(v), "to_int") => Ok(Val::Pure(Const::Int(i64::from_str_radix(v, 10)?))),
            (obj, name) => bail!("unimplemented method {obj:?}.{name:?} {pos:?} {kw:?}"),
        }
    }

    fn args(&mut self, args: &Args) -> eyre::Result<(Vec<Val<G::Id>>, Vec<(String, Val<G::Id>)>)> {
        let pos = args
            .pos
            .iter()
            .map(|v| self.atom(v))
            .collect::<eyre::Result<_>>()?;

        let kw = args
            .kw
            .iter()
            .map(|(k, v)| Ok((k.clone(), self.atom(v)?)))
            .collect::<eyre::Result<_>>()?;

        Ok((pos, kw))
    }

    fn call_project(&mut self, name: String, version: Option<String>) -> eyre::Result<Val<G::Id>> {
        self.project = Some(Project { name, version });
        Ok(Val::Pure(Const::Unset))
    }
    */
}

/*
fn index_const(base: &Const, index: &Const) -> eyre::Result<Const> {
    match (base, index) {
        (Const::Array(v), Const::Int(i)) => {
            let n = v.len() as i64;
            let j = if *i < 0 { n + *i } else { *i };
            v.get(j as usize)
                .cloned()
                .ok_or_eyre("index {i} out of range (len {n})")
        }
        (base, index) => bail!("unimplemented index on {base:?} {index:?}"),
    }
}
*/

#[derive(Debug, Clone)]
struct ArgSet {
    pub pos: Vec<Value>,
    pub kw: Vec<(Rc<str>, Value)>,
}
