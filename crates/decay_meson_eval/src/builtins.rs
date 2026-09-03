use {
    crate::{
        Interp,
        args::CallArgs,
        obj::{
            ConfigData,
            Dep,
            Entry,
            Module,
            Obj,
            Program, //
        },
        ops::join_paths,
        oracle::Pinned,
        val::Value,
    },
    decay_build_ir::{
        CmdArg,
        External,
        Install,
        Kind,
        Linkage,
        Source,
        Test, //
    },
    decay_meson_ast::{
        Loc,
        ProjectOption,
        ProjectOptionKind, //
    },
    decay_meson_logic::{
        Pc,
        Solver,
        Variant,
        Variational, //
    },
    eyre::{
        OptionExt,
        bail, //
    },
    std::{
        path::PathBuf,
        rc::Rc,
        str::FromStr, //
    },
    tracing::{
        debug,
        warn, //
    },
};

impl<'a, S: Solver> Interp<'a, S> {
    pub(crate) fn call(
        &mut self,
        name: &str,
        args: &CallArgs,
        loc: Loc,
    ) -> eyre::Result<Variational<Value>> {
        match name {
            "project" => self.fn_project(args),
            "get_option" => self.fn_get_option(args),
            "option" => bail!("`option()` belongs in the option file, not in meson.build"),

            // -- targets --
            "library" => self.fn_library(args, None),
            "static_library" => self.fn_library(args, Some(Linkage::Static)),
            "shared_library" | "shared_module" => self.fn_library(args, Some(Linkage::Shared)),
            "both_libraries" => self.fn_library(args, Some(Linkage::Both)),
            "executable" => self.fn_executable(args),
            "custom_target" => self.fn_custom_target(args),
            "configure_file" => self.fn_configure_file(args),
            "declare_dependency" => self.fn_declare_dependency(args),
            "alias_target" => Ok(self.pure(Value::Unset)),

            // -- looking things up --
            "dependency" => self.fn_dependency(args),
            "find_program" => self.fn_find_program(args),
            "files" => self.fn_files(args),
            "include_directories" => self.fn_include_directories(args),
            "import" => self.fn_import(args),
            "subproject" => bail!("subprojects are not supported yet"),

            // -- structure --
            "subdir" => self.fn_subdir(args),
            "test" | "benchmark" => self.fn_test(args),
            "install_headers" => self.fn_install(args, "include"),
            "install_data" => self.fn_install(args, "share"),
            "install_man" => self.fn_install(args, "share/man"),
            "install_subdir" => {
                self.warn_unsupported("install_subdir()", loc);
                Ok(self.pure(Value::Unset))
            }

            // -- values --
            "configuration_data" => {
                let v = self.config_data();
                Ok(self.pure(v))
            }
            "environment" => Ok(self.pure(Value::Obj(Obj::Env))),
            "join_paths" => self.fn_join_paths(args),
            "disabler" => bail!("`disabler()` is not supported"),
            "is_disabler" => Ok(self.bool_value(Pc::FALSE)),

            // -- diagnostics and flow --
            "error" => self.fn_error(args),
            "assert" => self.fn_assert(args),
            "warning" | "message" | "debug" | "summary" => {
                if let Some(first) = args.at(0) {
                    if let Ok(text) = self.stringify(first) {
                        for v in text.variants() {
                            debug!(target: "meson", "{}: {}", name, v.value);
                        }
                    }
                }
                Ok(self.pure(Value::Unset))
            }

            // -- variables --
            "is_variable" => {
                let name = self.one_string(args.at(0).ok_or_eyre("expected a name")?)?;
                let known = self.vars.contains_key(&*name);
                Ok(self.bool_value(if known { self.pc } else { Pc::FALSE }))
            }
            "get_variable" => {
                let want = self.one_string(args.at(0).ok_or_eyre("expected a name")?)?;
                match self.lookup(&want)? {
                    Some(v) => Ok(v),
                    None => args
                        .at(1)
                        .cloned()
                        .ok_or_else(|| eyre::eyre!("undefined variable `{want}`")),
                }
            }
            "set_variable" => {
                let want = self.one_string(args.at(0).ok_or_eyre("expected a name")?)?;
                let value = args.at(1).cloned().ok_or_eyre("expected a value")?;
                self.assign(&want, value);
                Ok(self.pure(Value::Unset))
            }

            // -- global compiler flags --
            "add_project_arguments"
            | "add_global_arguments"
            | "add_project_link_arguments"
            | "add_global_link_arguments"
            | "add_test_setup"
            | "add_languages" => {
                self.warn_unsupported(&format!("`{name}()`"), loc);
                Ok(self.pure(Value::Unset))
            }

            "run_command" => bail!(
                "`run_command()` would have to run a program at import time, which would \
                 bake this machine's answer into the generated build"
            ),

            _ => bail!("unimplemented function `{name}()`"),
        }
    }

    // -- project ----------------------------------------------------------

    fn fn_project(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        if self.has_project {
            bail!("project() was called twice");
        }
        self.has_project = true;

        let name = self.one_string(args.at(0).ok_or_eyre("project() needs a name")?)?;
        self.graph.project.name = name.to_string();

        let mut languages = Vec::new();
        for arg in args.rest() {
            for s in self.strings(arg)?.into_variants() {
                languages.push(s.value.to_string());
            }
        }
        if let Some(v) = args.get("language") {
            for s in self.strings(v)?.into_variants() {
                languages.push(s.value.to_string());
            }
        }
        self.graph.project.languages = languages;

        self.graph.project.version = self.opt_string(args, "version")?.map(|v| v.to_string());

        if let Some(v) = args.get("license") {
            self.graph.project.license = self
                .strings(v)?
                .into_variants()
                .map(|v| v.value.to_string())
                .collect();
        }

        // `default_options:` changes what an option resolves to when nobody
        // overrides it, which the backend surfaces as the default configuration.
        if let Some(v) = args.get("default_options") {
            for entry in self.strings(v)?.into_variants() {
                let Some((key, value)) = entry.value.split_once('=') else {
                    bail!("`{}` is not a `key=value` default option", entry.value);
                };
                self.default_options
                    .insert(key.trim().to_owned(), Rc::from(value.trim()));
            }
        }

        Ok(self.pure(Value::Unset))
    }

    // -- options ----------------------------------------------------------

    fn fn_get_option(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = self.one_string(args.at(0).ok_or_eyre("get_option() needs a name")?)?;

        // Anything the importer pinned is answered straight away and never
        // reaches the configuration space.
        if let Some(pinned) = self.oracle.option(&name) {
            return Ok(self.pure(pinned_value(&pinned)));
        }

        let decl = self
            .option_decl(&name)
            .ok_or_else(|| eyre::eyre!("unknown build option `{name}`"))?;

        if let Some(id) = self.option_var(&name)? {
            let n = self.logic.var(id).choices.len() as u32;
            let choices = self.logic.var(id).choices.clone();
            let mut out = Variational::empty();
            for (i, choice) in choices.iter().enumerate().take(n as usize) {
                let lit = self.logic.lit(id, i as u32);
                let cond = self.logic.and(self.pc, lit);
                if cond.is_false() {
                    continue;
                }
                out.push(Variant::new(cond, option_choice(&decl.kind, choice)));
            }
            out.normalize(&mut self.logic);
            return Ok(out);
        }

        // No finite domain to branch over: use the default, and say so, because
        // the generated build will have that value baked in.
        let value = self.option_default(&name, &decl)?;
        debug!(%name, "option has no finite domain; using its default");
        Ok(self.pure(value))
    }

    fn option_default(&mut self, name: &str, decl: &ProjectOption) -> eyre::Result<Value> {
        let overridden = self.default_options.get(name).cloned();
        Ok(match &decl.kind {
            ProjectOptionKind::String { value } => {
                Value::Str(overridden.unwrap_or_else(|| Rc::from(value.as_str())))
            }
            ProjectOptionKind::Integer { value } => match overridden {
                Some(v) => Value::Int(v.parse()?),
                None => Value::Int(*value),
            },
            ProjectOptionKind::Array { value, .. } => {
                let items = match overridden {
                    Some(v) => v.split(',').map(|s| s.trim().to_owned()).collect(),
                    None => value.clone(),
                };
                let pc = self.pc;
                Value::list(
                    items
                        .into_iter()
                        .map(|s| Variant::new(pc, Value::from(s)))
                        .collect(),
                )
            }
            ProjectOptionKind::Bool { value } => Value::Bool(*value),
            ProjectOptionKind::Combo { value, .. } => Value::str(value),
            ProjectOptionKind::Feature { value } => Value::Obj(Obj::Feature(Rc::from(value.as_str()))),
        })
    }

    // -- targets ----------------------------------------------------------

    fn fn_library(
        &mut self,
        args: &CallArgs,
        linkage: Option<Linkage>,
    ) -> eyre::Result<Variational<Value>> {
        // A plain `library()` follows `default_library`, which is usually still
        // open, so the linkage is variational just like everything else.
        let kind = match linkage {
            Some(Linkage::Static) => Kind::StaticLibrary,
            Some(Linkage::Shared) => Kind::SharedLibrary,
            Some(Linkage::Both) => Kind::Library {
                linkage: Variational::pure(Linkage::Both),
            },
            None => Kind::Library {
                linkage: self.default_linkage()?,
            },
        };
        self.build_target(args, kind)
    }

    fn fn_executable(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        self.build_target(args, Kind::Executable)
    }

    fn default_linkage(&mut self) -> eyre::Result<Variational<Linkage>> {
        let args = CallArgs {
            pos: vec![self.pure(Value::str("default_library"))],
            kw: Vec::new(),
        };
        let value = self.fn_get_option(&args)?;
        let mut out = Variational::empty();
        for variant in value.variants() {
            let name = variant
                .value
                .as_str()
                .ok_or_eyre("`default_library` should be a string")?;
            let linkage = match &**name {
                "static" => Linkage::Static,
                "shared" => Linkage::Shared,
                "both" => Linkage::Both,
                other => bail!("unknown `default_library` value `{other}`"),
            };
            out.push(Variant::new(variant.cond, linkage));
        }
        Ok(out)
    }

    fn build_target(&mut self, args: &CallArgs, kind: Kind) -> eyre::Result<Variational<Value>> {
        let name = self.one_string(args.at(0).ok_or_eyre("a target needs a name")?)?;
        let dir = self.cur_dir().to_path_buf();
        let id = self.graph.add(&name, &dir, self.pc, kind);

        let mut srcs = Variational::empty();
        for arg in args.rest() {
            srcs.extend(self.sources(arg)?);
        }
        if let Some(v) = args.get("sources") {
            srcs.extend(self.sources(v)?);
        }
        // Headers listed as sources are not compiled; splitting them out here
        // keeps the backend from having to guess by extension.
        let (srcs, headers) = self.split_headers(srcs);

        let mut deps = Variational::empty();
        if let Some(v) = args.get("dependencies") {
            deps.extend(self.deps(v)?);
        }

        let mut link_with = Variational::empty();
        for key in ["link_with", "link_whole"] {
            if let Some(v) = args.get(key) {
                link_with.extend(self.deps(v)?);
            }
        }

        let mut include_dirs = Variational::empty();
        if let Some(v) = args.get("include_directories") {
            include_dirs.extend(self.include_dirs(v)?);
        }

        let mut compile_args = Variational::empty();
        for key in ["c_args", "cpp_args", "objc_args", "objcpp_args", "args"] {
            if let Some(v) = args.get(key) {
                compile_args.extend(self.strings(v)?.map(|s| s.to_string()));
            }
        }

        let mut link_args = Variational::empty();
        if let Some(v) = args.get("link_args") {
            link_args.extend(self.strings(v)?.map(|s| s.to_string()));
        }

        let install = self.flag(args, "install", Pc::FALSE)?;
        let version = self.opt_string(args, "version")?.map(|v| v.to_string());

        let target = self.graph.target_mut(id);
        target.attrs.srcs = srcs;
        target.attrs.headers = headers;
        target.attrs.deps = deps;
        target.attrs.link_with = link_with;
        target.attrs.include_dirs = include_dirs;
        target.attrs.compile_args = compile_args;
        target.attrs.link_args = link_args;
        target.attrs.install = install;
        target.attrs.version = version;

        Ok(self.pure(Value::Obj(Obj::Target(id))))
    }

    fn fn_custom_target(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = match args.at(0) {
            Some(v) => self.one_string(v)?,
            None => Rc::from("custom_target"),
        };
        let dir = self.cur_dir().to_path_buf();
        let id = self.graph.add(&name, &dir, self.pc, Kind::Custom);

        let mut srcs = Variational::empty();
        if let Some(v) = args.get("input") {
            srcs.extend(self.sources(v)?);
        }

        let outs = match args.get("output") {
            Some(v) => self
                .strings(v)?
                .into_variants()
                .map(|v| v.value.to_string())
                .collect(),
            None => bail!("custom_target() needs an `output:`"),
        };

        let mut cmd = Variational::empty();
        if let Some(v) = args.get("command") {
            cmd.extend(self.command(v)?);
        }

        let mut deps = Variational::empty();
        for key in ["depends", "depend_files"] {
            if let Some(v) = args.get(key) {
                if let Ok(d) = self.deps(v) {
                    deps.extend(d);
                }
            }
        }

        let install = self.flag(args, "install", Pc::FALSE)?;
        let install_dir = self.opt_string(args, "install_dir")?.map(|v| v.to_string());

        let target = self.graph.target_mut(id);
        target.attrs.srcs = srcs;
        target.attrs.outs = outs;
        target.attrs.cmd = cmd;
        target.attrs.deps = deps;
        target.attrs.install = install;
        target.attrs.install_dir = install_dir;

        Ok(self.pure(Value::Obj(Obj::Target(id))))
    }

    fn fn_configure_file(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let output = self
            .opt_string(args, "output")?
            .ok_or_eyre("configure_file() needs an `output:`")?;
        let dir = self.cur_dir().to_path_buf();
        let id = self.graph.add(&output, &dir, self.pc, Kind::ConfigHeader);

        let template = match args.get("input") {
            Some(v) => self.sources(v)?.variants().first().map(|v| v.value.clone()),
            None => None,
        };

        let defines = match args.get("configuration") {
            Some(v) => self.defines(v)?,
            None => Variational::empty(),
        };

        let install = self.flag(args, "install", Pc::FALSE)?;
        let install_dir = self.opt_string(args, "install_dir")?.map(|v| v.to_string());

        let target = self.graph.target_mut(id);
        target.attrs.outs = vec![output.to_string()];
        target.attrs.defines = defines;
        target.attrs.template = template;
        target.attrs.install = install;
        target.attrs.install_dir = install_dir;

        Ok(self.pure(Value::Obj(Obj::Target(id))))
    }

    /// Read a `configuration_data()` object into header entries.
    fn defines(&mut self, v: &Variational<Value>) -> eyre::Result<Variational<decay_build_ir::Define>> {
        use decay_build_ir::{Define, DefineValue};

        let mut out = Variational::empty();
        for variant in v.variants() {
            let Value::Obj(Obj::ConfigData(data)) = &variant.value else {
                bail!(
                    "`configuration:` expects configuration_data(), found a {}",
                    variant.value.type_name()
                );
            };
            let data: ConfigData = data.borrow().clone();
            for (name, entries) in &data.entries {
                for entry in entries.variants() {
                    let cond = self.logic.and(variant.cond, entry.cond);
                    if cond.is_false() {
                        continue;
                    }
                    let value = match &entry.value {
                        Entry::Quoted(v) => DefineValue::Quoted(v.to_string()),
                        Entry::Raw(v) => DefineValue::Raw(v.to_string()),
                        Entry::Int(v) => DefineValue::Number(*v),
                        Entry::Ten(v) => DefineValue::Number(i64::from(*v)),
                        Entry::Flag(true) => DefineValue::Flag,
                        Entry::Flag(false) => DefineValue::Undef,
                    };
                    out.push(Variant::new(cond, Define {
                        name: name.to_string(),
                        value,
                    }));
                }
            }
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }

    fn command(&mut self, v: &Variational<Value>) -> eyre::Result<Variational<CmdArg>> {
        let mut out = Variational::empty();
        for variant in self.flat(v) {
            let arg = match &variant.value {
                Value::Str(s) => match &**s {
                    "@INPUT@" => CmdArg::Inputs,
                    "@OUTPUT@" => CmdArg::Outputs,
                    "@OUTDIR@" => CmdArg::OutDir,
                    other => CmdArg::Literal(other.to_owned()),
                },
                Value::Obj(Obj::Program(p)) => CmdArg::Target(p.target),
                Value::Obj(Obj::Target(id)) | Value::Obj(Obj::Output(id, _)) => CmdArg::Target(*id),
                Value::Obj(Obj::File(path)) => CmdArg::File(PathBuf::from(&**path)),
                other => bail!("cannot use a {} in a command", other.type_name()),
            };
            out.push(Variant::new(variant.cond, arg));
        }
        Ok(out)
    }

    fn fn_declare_dependency(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = format!("{}-dep", self.graph.project.name);
        let dir = self.cur_dir().to_path_buf();
        let id = self.graph.add(&name, &dir, self.pc, Kind::Interface);

        let mut link_with = Variational::empty();
        for key in ["link_with", "link_whole"] {
            if let Some(v) = args.get(key) {
                link_with.extend(self.deps(v)?);
            }
        }

        let mut deps = Variational::empty();
        if let Some(v) = args.get("dependencies") {
            deps.extend(self.deps(v)?);
        }

        let mut include_dirs = Variational::empty();
        if let Some(v) = args.get("include_directories") {
            include_dirs.extend(self.include_dirs(v)?);
        }

        let mut compile_args = Variational::empty();
        if let Some(v) = args.get("compile_args") {
            compile_args.extend(self.strings(v)?.map(|s| s.to_string()));
        }

        let mut link_args = Variational::empty();
        if let Some(v) = args.get("link_args") {
            link_args.extend(self.strings(v)?.map(|s| s.to_string()));
        }

        let mut headers = Variational::empty();
        if let Some(v) = args.get("sources") {
            headers.extend(self.sources(v)?);
        }

        let mut variables = Variational::empty();
        if let Some(v) = args.get("variables") {
            variables.extend(self.pairs(v)?);
        }

        let target = self.graph.target_mut(id);
        target.attrs.link_with = link_with;
        target.attrs.deps = deps;
        target.attrs.include_dirs = include_dirs;
        target.attrs.compile_args = compile_args;
        target.attrs.link_args = link_args;
        target.attrs.headers = headers;
        target.attrs.variables = variables;

        let value = self.dep_obj(Dep {
            name,
            found: Pc::TRUE,
            target: id,
            type_name: "internal",
            version: None,
            variables: Vec::new(),
        });
        Ok(self.pure(value))
    }

    /// A `variables:` argument, which meson accepts as either a dict or a list
    /// of `key=value` strings.
    fn pairs(&mut self, v: &Variational<Value>) -> eyre::Result<Variational<(String, String)>> {
        let mut out = Variational::empty();
        for variant in v.variants() {
            if let Value::Dict(entries) = &variant.value {
                for entry in entries.iter() {
                    let cond = self.logic.and(variant.cond, entry.cond);
                    if cond.is_false() {
                        continue;
                    }
                    let value = entry
                        .value
                        .1
                        .as_str()
                        .ok_or_eyre("variable values should be strings")?;
                    out.push(Variant::new(cond, (entry.value.0.to_string(), value.to_string())));
                }
                continue;
            }
            let flat: Variational<Value> = Variant::new(variant.cond, variant.value.clone()).into();
            for s in self.strings(&flat)?.into_variants() {
                let Some((k, v)) = s.value.split_once('=') else {
                    bail!("`{}` is not a `key=value` variable", s.value);
                };
                out.push(Variant::new(s.cond, (k.to_owned(), v.to_owned())));
            }
        }
        Ok(out)
    }

    // -- looking things up ------------------------------------------------

    fn fn_dependency(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = self.one_string(args.at(0).ok_or_eyre("dependency() needs a name")?)?;
        let required = self.required(args)?;

        let (key, kind, type_name) = if &*name == "appleframeworks" {
            let modules: Vec<String> = match args.get("modules") {
                Some(v) => self
                    .strings(v)?
                    .into_variants()
                    .map(|v| v.value.to_string())
                    .collect(),
                None => Vec::new(),
            };
            (
                format!("dep:frameworks:{}", modules.join(",")),
                External::Framework { modules },
                "appleframeworks",
            )
        } else {
            (
                format!("dep:{name}"),
                External::PkgConfig {
                    module: name.to_string(),
                },
                "pkgconfig",
            )
        };

        let target = self.external(&key, &name, kind);
        let found = self.dependency_found(&key, &name, required);

        let value = self.dep_obj(Dep {
            name: name.to_string(),
            found,
            target,
            type_name,
            version: None,
            variables: Vec::new(),
        });
        Ok(self.pure(value))
    }

    /// The condition under which a looked-up dependency is available.
    ///
    /// Where it was `required:`, a build that does not have it fails to
    /// configure at all — so rather than tracking a "found" flag that can never
    /// be false there, the configuration space is narrowed to say so.
    pub(crate) fn dependency_found(&mut self, key: &str, name: &str, required: Pc) -> Pc {
        let found = self.probe(key, format!("`{name}` is available"));
        if !required.is_false() {
            let must = self.logic.implies(required, found);
            self.logic.assume(must);
        }
        found
    }

    /// The `required:` argument as a condition.
    fn required(&mut self, args: &CallArgs) -> eyre::Result<Pc> {
        let Some(v) = args.get("required") else {
            return Ok(self.pc);
        };
        // `required:` also accepts a feature option.
        let mut cond = Pc::FALSE;
        let mut plain = Variational::empty();
        for variant in v.variants() {
            match &variant.value {
                Value::Obj(Obj::Feature(f)) => {
                    if &**f == "enabled" {
                        cond = self.logic.or(cond, variant.cond);
                    }
                }
                other => plain.push(Variant::new(variant.cond, other.clone())),
            }
        }
        if !plain.is_empty() {
            let t = self.truth(&plain)?;
            cond = self.logic.or(cond, t);
        }
        Ok(self.logic.and(self.pc, cond))
    }

    fn fn_find_program(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = self.one_string(args.at(0).ok_or_eyre("find_program() needs a name")?)?;
        let required = self.required(args)?;

        // A program named by a path inside the project is a file, not something
        // to go looking for on the build machine.
        let candidate = self.resolve(&name);
        let in_tree = self.sources.exists(&self.root.join(&candidate));
        let path = in_tree.then(|| PathBuf::from(&candidate));

        let key = format!("prog:{name}");
        let target = self.external(&key, &name, External::Program {
            name: name.to_string(),
            path: path.clone(),
        });

        let found = if in_tree || self.oracle.has_program(&name) {
            Pc::TRUE
        } else {
            // Not a configuration knob: nothing a platform could set would make
            // a program appear, because the build never looks outside its own
            // graph for one. So it is absent, and the configurations that
            // insisted on it are the ones that cannot be configured.
            if required.is_true() {
                bail!(
                    "find_program('{name}') is required, but it is not in the project and \
                     nothing supplies it; add `programs.{name} = \"//some:target\"` to the \
                     importer configuration"
                );
            }
            let optional = self.logic.not(required);
            self.logic.assume(optional);
            Pc::FALSE
        };

        let value = self.program_obj(Program {
            name: name.to_string(),
            found,
            target,
            path: path.map(|p| p.display().to_string()),
        });
        Ok(self.pure(value))
    }

    fn fn_files(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let mut items = Vec::new();
        for arg in &args.pos {
            for variant in self.strings(arg)?.into_variants() {
                let path = self.resolve(&variant.value);
                if !self.sources.exists(&self.root.join(&path)) {
                    warn!(%path, "files() names a path that is not in the source tree");
                }
                items.push(Variant::new(
                    variant.cond,
                    Value::Obj(Obj::File(Rc::from(path.as_str()))),
                ));
            }
        }
        Ok(self.pure(Value::list(items)))
    }

    fn fn_include_directories(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let mut dirs = Vec::new();
        for arg in &args.pos {
            for variant in self.strings(arg)?.into_variants() {
                dirs.push(self.resolve(&variant.value));
            }
        }
        Ok(self.pure(Value::Obj(Obj::IncludeDirs(Rc::new(dirs)))))
    }

    fn fn_import(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = self.one_string(args.at(0).ok_or_eyre("import() needs a module name")?)?;
        let module = Module::from_str(&name)?;
        Ok(self.pure(Value::Obj(Obj::Module(module))))
    }

    fn fn_join_paths(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let mut segments: Vec<Variational<Rc<str>>> = Vec::new();
        for arg in &args.pos {
            segments.push(self.strings(arg)?);
        }

        // Each segment may itself differ between configurations, so the join is
        // taken over the product of the segments that can co-occur.
        let mut out: Variational<String> = Variant::new(self.pc, String::new()).into();
        for segment in &segments {
            let mut next = Variational::empty();
            for base in out.variants() {
                for part in segment.variants() {
                    let cond = self.logic.and(base.cond, part.cond);
                    if cond.is_false() {
                        continue;
                    }
                    next.push(Variant::new(cond, join_paths([base.value.as_str(), &part.value])));
                }
            }
            next.normalize(&mut self.logic);
            out = next;
        }

        let mut values = out.map(Value::from);
        values.normalize(&mut self.logic);
        Ok(values)
    }

    // -- structure --------------------------------------------------------

    fn fn_subdir(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = self.one_string(args.at(0).ok_or_eyre("subdir() needs a directory")?)?;
        let dir = PathBuf::from(self.resolve(&name));
        self.subdir(&dir)?;
        Ok(self.pure(Value::Unset))
    }

    fn fn_test(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let name = self.one_string(args.at(0).ok_or_eyre("test() needs a name")?)?;
        let exe = args.at(1).ok_or_eyre("test() needs an executable")?;

        let mut targets = self.deps(exe)?;
        targets.normalize(&mut self.logic);

        let mut cmd_args = Variational::empty();
        if let Some(v) = args.get("args") {
            cmd_args.extend(self.strings(v)?.map(|s| s.to_string()));
        }

        for variant in targets.variants() {
            self.graph.tests.push(Test {
                name: name.to_string(),
                target: variant.value,
                cond: variant.cond,
                args: cmd_args.clone(),
            });
        }
        Ok(self.pure(Value::Unset))
    }

    fn fn_install(&mut self, args: &CallArgs, default_dir: &str) -> eyre::Result<Variational<Value>> {
        let mut files = Variational::empty();
        for arg in &args.pos {
            files.extend(self.sources(arg)?);
        }
        let subdir = self
            .opt_string(args, "subdir")?
            .map(|v| join_paths([default_dir, &v]))
            .or_else(|| Some(default_dir.to_owned()));

        self.graph.installs.push(Install {
            files,
            subdir,
            cond: self.pc,
        });
        Ok(self.pure(Value::Unset))
    }

    // -- diagnostics ------------------------------------------------------

    fn fn_error(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let mut text = Vec::new();
        for arg in &args.pos {
            if let Ok(s) = self.stringify(arg) {
                text.extend(s.into_variants().map(|v| v.value.to_string()));
            }
        }
        debug!(pc = self.pc.index(), "error({})", text.join(" "));

        // Configurations that reach `error()` do not configure, so they are
        // removed from the space rather than reported as a failure.
        let dead = self.pc;
        let alive = self.logic.not(dead);
        self.logic.assume(alive);
        self.abort();
        Ok(self.pure(Value::Unset))
    }

    fn fn_assert(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let cond = args.at(0).ok_or_eyre("assert() needs a condition")?.clone();
        let holds = self.truth(&cond)?;
        let must = self.logic.implies(self.pc, holds);
        self.logic.assume(must);
        Ok(self.pure(Value::Unset))
    }

    /// Split a source list into things that get compiled and things that only
    /// get included.
    ///
    /// Meson accepts headers in `sources:` and works out which is which; the
    /// distinction matters downstream, so it is made once, here.
    fn split_headers(
        &self,
        srcs: Variational<Source>,
    ) -> (Variational<Source>, Variational<Source>) {
        let mut compiled = Variational::empty();
        let mut headers = Variational::empty();
        for variant in srcs {
            let name = match &variant.value {
                Source::File(path) => path.clone(),
                Source::Generated(id) => {
                    let target = self.graph.target(*id);
                    PathBuf::from(target.attrs.outs.first().cloned().unwrap_or_default())
                }
            };
            let is_header = matches!(
                name.extension().and_then(|e| e.to_str()),
                Some("h" | "hh" | "hpp" | "hxx" | "inc" | "def")
            );
            if is_header {
                headers.push(variant);
            } else {
                compiled.push(variant);
            }
        }
        (compiled, headers)
    }

    /// A boolean keyword argument, as a condition.
    pub(crate) fn flag(
        &mut self,
        args: &CallArgs,
        name: &str,
        absent: Pc,
    ) -> eyre::Result<Pc> {
        let Some(v) = args.get(name) else {
            return Ok(absent);
        };
        let t = self.truth(v)?;
        Ok(self.logic.and(self.pc, t))
    }
}

fn pinned_value(pinned: &Pinned) -> Value {
    match pinned {
        Pinned::Bool(v) => Value::Bool(*v),
        Pinned::Int(v) => Value::Int(*v),
        Pinned::Str(v) => Value::Str(v.clone()),
        Pinned::List(items) => Value::list(
            items
                .iter()
                .map(|s| Variant::new(Pc::TRUE, Value::Str(s.clone())))
                .collect(),
        ),
    }
}

/// The value an option takes when its variable landed on `choice`.
fn option_choice(kind: &ProjectOptionKind, choice: &str) -> Value {
    match kind {
        ProjectOptionKind::Bool { .. } => Value::Bool(choice == "true"),
        ProjectOptionKind::Feature { .. } => Value::Obj(Obj::Feature(Rc::from(choice))),
        _ => Value::str(choice),
    }
}

/// Meson's own build options, which no project declares but every project can
/// read.
pub(crate) fn builtin_option(name: &str) -> Option<ProjectOption> {
    let kind = match name {
        "prefix" => str_opt("/usr/local"),
        "bindir" => str_opt("bin"),
        "datadir" => str_opt("share"),
        "includedir" => str_opt("include"),
        "infodir" => str_opt("share/info"),
        "libdir" => str_opt("lib"),
        "libexecdir" => str_opt("libexec"),
        "licensedir" => str_opt(""),
        "localedir" => str_opt("share/locale"),
        "localstatedir" => str_opt("var"),
        "mandir" => str_opt("share/man"),
        "sbindir" => str_opt("sbin"),
        "sharedstatedir" => str_opt("com"),
        "sysconfdir" => str_opt("etc"),

        "default_library" => combo(&["shared", "static", "both"], "shared"),
        "buildtype" => combo(
            &["plain", "debug", "debugoptimized", "release", "minsize", "custom"],
            "debug",
        ),
        "optimization" => combo(&["plain", "0", "g", "1", "2", "3", "s"], "0"),
        "warning_level" => combo(&["0", "1", "2", "3", "everything"], "1"),
        "b_ndebug" => combo(&["true", "false", "if-release"], "false"),
        "b_vscrt" => combo(&["none", "md", "mdd", "mt", "mtd", "from_buildtype"], "from_buildtype"),
        "layout" => combo(&["mirror", "flat"], "mirror"),
        "wrap_mode" => combo(
            &["default", "nofallback", "nodownload", "forcefallback", "nopromote"],
            "default",
        ),
        "backend" => combo(&["ninja", "vs", "xcode", "none"], "ninja"),
        "c_std" | "cpp_std" | "objc_std" | "objcpp_std" => combo(
            &[
                "none", "c89", "c99", "c11", "c17", "c18", "c2x", "gnu89", "gnu99", "gnu11",
                "gnu17", "gnu18", "gnu2x", "c++98", "c++11", "c++14", "c++17", "c++20",
                "gnu++11", "gnu++14", "gnu++17", "gnu++20",
            ],
            "none",
        ),
        "unity" => combo(&["on", "off", "subprojects"], "off"),

        "debug" => bool_opt(true),
        "strip" | "werror" | "prefer_static" | "b_lto" | "b_coverage" | "b_pie" | "vsenv" => {
            bool_opt(false)
        }
        "b_staticpic" | "b_asneeded" | "b_lundef" => bool_opt(true),

        "c_args" | "cpp_args" | "c_link_args" | "cpp_link_args" | "pkg_config_path"
        | "cmake_prefix_path" => ProjectOptionKind::Array {
            choices: Vec::new(),
            value: Vec::new(),
        },

        _ => return None,
    };

    // No description: that it is one of meson's own is what the caller needs to
    // know, and that is carried by the variable's kind.
    Some(ProjectOption {
        description: None,
        kind,
        deprecated: false,
    })
}

fn str_opt(value: &str) -> ProjectOptionKind {
    ProjectOptionKind::String {
        value: value.to_owned(),
    }
}

fn bool_opt(value: bool) -> ProjectOptionKind {
    ProjectOptionKind::Bool { value }
}

fn combo(choices: &[&str], value: &str) -> ProjectOptionKind {
    ProjectOptionKind::Combo {
        choices: choices.iter().map(|s| s.to_string()).collect(),
        value: value.to_owned(),
    }
}
