use {
    crate::ast::{
        Args, Block, Call, Expr, Method, Stmt,
        sym::{Env, Setting, SettingId},
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
        path::PathBuf,
        rc::Rc, //
    },
};

#[derive(Debug)]
pub struct Interp<'a> {
    systems: &'a HashMap<String, String>,
    env: Env,
    project: Project,
    vars: HashMap<String, Val>,
}

impl<'a> Interp<'a> {
    pub fn new(systems: &'a HashMap<String, String>) -> Self {
        let mut vars = HashMap::new();
        vars.insert("meson".into(), Val::Obj(Rc::new(Obj::Meson)));
        vars.insert(
            "host_machine".into(),
            Val::Obj(Rc::new(Obj::Machine(MachineKind::Host))),
        );

        Self {
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
        }
    }

    pub fn exec_block(&mut self, block: &Block) -> eyre::Result<()> {
        for stmt in &block.0 {
            self.exec_stmt(stmt)?;
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
                assert!(!assign.is_plus, "unimplemented");
                self.vars.insert(assign.name.clone(), value);
            }
            _ => todo!("{stmt:?}"),
        }

        Ok(())
    }

    fn eval(&mut self, expr: &Expr) -> eyre::Result<Val> {
        match expr {
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
            choices: vec!["true".into(), "false".into()],
        })?;
        Ok(id)
    }
}

#[derive(Debug, Clone)]
enum Val {
    String(String),
    Obj(Rc<Obj>),
    Array(Vec<Val>),
    Int(i64),
    Sym(SettingId),
    Unset,
}

impl Val {
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

#[derive(Debug, Clone)]
enum Obj {
    Compiler(Lang),
    Machine(MachineKind),
    Meson,
    CfgData(HashMap<String, String>),
}

#[derive(Debug, Clone)]
enum Lang {
    C,
}

#[derive(Debug, Clone)]
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
