use {
    crate::ast::{
        Args,
        Block,
        Call,
        Expr,
        Method,
        Stmt, //
    },
    eyre::{Ok, OptionExt, bail},
    std::{
        collections::{BTreeMap, HashMap},
        rc::Rc,
    },
};

#[derive(Debug)]
pub struct Interp {
    project: Project,
    vars: HashMap<String, Val>,
}

impl Interp {
    pub fn new() -> Self {
        let mut vars = HashMap::new();
        vars.insert("meson".into(), Val::Obj(Rc::new(Obj::Meson)));

        Self {
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
            Expr::Array(array) => Ok(Val::Array(
                array
                    .iter()
                    .map(|v| self.eval(v))
                    .collect::<eyre::Result<_>>()?,
            )),
            _ => bail!("{expr:?}"),
        }
    }

    fn call(&mut self, call: &Call) -> eyre::Result<Val> {
        match call.name.as_str() {
            "project" => {
                let (positional, keyword) = self.eval_args(&call.args)?;
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
            _ => bail!("Unknown function call {}", call.name),
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
                _ => bail!("Unknown method `{}`", method.name),
            }
        }
        Ok(Val::Unset)
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
}

#[derive(Debug, Clone)]
enum Val {
    String(String),
    Obj(Rc<Obj>),
    Array(Vec<Val>),
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
    Meson,
}

#[derive(Debug)]
struct Project {
    name: String,
    languages: Vec<String>,
    version: Option<String>,
    default_options: Option<Vec<String>>,
    license: Option<String>,
}
