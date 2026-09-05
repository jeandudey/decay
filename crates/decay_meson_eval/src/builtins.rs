use {
    crate::{
        Interp,
        args::CallArgs,
        obj::{
            ConfigData,
            Dep,
            Entry,
            Lang,
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
        Define,
        DefineValue,
        External,
        Install,
        Kind,
        Linkage,
        Package,
        Source,
        TargetId,
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
        collections::BTreeSet,
        path::{
            Path,
            PathBuf, //
        },
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
            // Subprojects are not evaluated. A project that pulls one in is
            // expected to have it listed as its own `[[project]]` instead, so
            // the `dependency()` the subproject would have provided resolves
            // against that sibling. The call itself does nothing here; code
            // that uses the returned subproject object will fail later, which
            // is the signal to add it to `decay.toml`.
            "subproject" => {
                self.warn_unsupported(&format!("`{name}()`"), loc);
                Ok(self.pure(Value::Unset))
            }

            // -- structure --
            "subdir" => self.fn_subdir(args),
            "subdir_done" => {
                self.subdir_done();
                Ok(self.pure(Value::Unset))
            }
            "test" | "benchmark" => self.fn_test(args),
            "install_headers" => self.fn_install(args, "include"),
            "install_data" => self.fn_install(args, "share"),
            "install_man" => self.fn_install(args, "share/man"),
            "install_subdir" => {
                self.warn_unsupported("install_subdir()", loc);
                Ok(self.pure(Value::Unset))
            }
            "install_symlink" | "install_emptydir" => {
                // A symlink or a bare directory created at install time, not a
                // file of its own; nothing installs anything yet, so there is
                // no graph to add it to.
                debug!("{name}() has no build-graph equivalent; skipping");
                Ok(self.pure(Value::Unset))
            }

            // -- values --
            "configuration_data" => {
                let v = self.config_data();
                Ok(self.pure(v))
            }
            "environment" => Ok(self.pure(Value::Obj(Obj::Env))),
            "join_paths" => self.fn_join_paths(args),
            "disabler" => Ok(self.pure(Value::Obj(Obj::Disabler))),
            "is_disabler" => {
                let arg = args.at(0).ok_or_eyre("expected a value")?;
                let mut out = Variational::empty();
                for variant in arg.variants() {
                    let is = matches!(variant.value, Value::Obj(Obj::Disabler));
                    out.push(Variant::new(variant.cond, Value::Bool(is)));
                }
                out.normalize(&mut self.logic);
                Ok(out)
            }

            // -- diagnostics and flow --
            "error" => self.fn_error(args),
            "assert" => self.fn_assert(args),
            "warning" | "message" | "debug" | "summary" => {
                if let Some(first) = args.at(0)
                    && let Ok(text) = self.stringify(first)
                {
                    for v in text.variants() {
                        debug!(target: "meson", "{}: {}", name, v.value);
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
            //
            // Not distinguished by `language:`, the same simplification a
            // target's own `c_args`/`cpp_args`/... already make: every
            // language's flags land in the one `compile_args` a target has.
            // `add_global_*` differs from `add_project_*` only for
            // subprojects, which this importer does not model as anything
            // other than an independent project of their own, so both are
            // handled the same way.
            "add_project_arguments" | "add_global_arguments" => {
                let flags = self.flag_args(args)?;
                self.project_args.extend(flags);
                Ok(self.pure(Value::Unset))
            }
            "add_project_link_arguments" | "add_global_link_arguments" => {
                let flags = self.flag_args(args)?;
                self.project_link_args.extend(flags);
                Ok(self.pure(Value::Unset))
            }
            "add_test_setup" => {
                self.warn_unsupported(&format!("`{name}()`"), loc);
                Ok(self.pure(Value::Unset))
            }

            // A name this executor already knows how to answer
            // `meson.get_compiler()` for is as available as any other
            // compiler capability defaults to; anything else genuinely
            // cannot be claimed.
            "add_languages" => {
                let mut known = true;
                for arg in &args.pos {
                    for s in self.strings(arg)?.into_variants() {
                        if Lang::from_str(&s.value).is_err() {
                            known = false;
                        }
                    }
                }
                let found = Pc::from_bool(known);
                let required = self.required(args)?;
                if !required.is_false() {
                    let must = self.logic.implies(required, found);
                    self.logic.assume(must);
                }
                Ok(self.bool_value(found))
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

        self.graph.project.version = if let Some(path) = self.opt_file(args, "version")? {
            let path = self
                .root
                .join(self.dirs.last().unwrap())
                .join(path.as_ref());
            self.sources.read(&path).map(Some)?
        } else {
            self.opt_string(args, "version")?.map(|v| v.to_string())
        };

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
        // reaches the configuration space. A `feature` option pinned as a
        // bare string in `decay.toml` still has to come back as a feature
        // object, so `.disabled()`/`.enabled()`/`.auto()` keep working —
        // `pinned_scalar` maps a string onto the declared kind, and is a
        // no-op for string/combo options.
        if let Some(pinned) = self.oracle.option(&name) {
            let kind = self.option_decl(&name).map(|d| d.kind);
            if let Pinned::ByConstraint {
                setting,
                domain,
                cases,
                default,
            } = &pinned
            {
                return self.option_by_constraint(
                    &name,
                    setting,
                    domain.clone(),
                    cases,
                    default.as_deref(),
                    kind.as_ref(),
                );
            }
            return Ok(self.pure(pinned_scalar(&pinned, kind.as_ref())));
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

    /// A `get_option()` pinned to one value per value of a constraint the
    /// build already selects on — the option counterpart of a `[sizeof]`
    /// table, keyed the same way and sharing the same constraint variable.
    fn option_by_constraint(
        &mut self,
        name: &str,
        setting: &str,
        domain: Vec<String>,
        cases: &[(String, Box<Pinned>)],
        default: Option<&Pinned>,
        kind: Option<&ProjectOptionKind>,
    ) -> eyre::Result<Variational<Value>> {
        let (id, choices) = self.constraint_var(setting, domain);
        let mut out = Variational::empty();
        let mut covered = vec![false; choices.len()];
        for (value, inner) in cases {
            let Some(ci) = choices.iter().position(|c| c == value) else {
                continue;
            };
            covered[ci] = true;
            let lit = self.logic.lit(id, ci as u32);
            let cond = self.logic.and(self.pc, lit);
            if cond.is_false() {
                continue;
            }
            out.push(Variant::new(cond, pinned_scalar(inner, kind)));
        }
        if let Some(def) = default {
            let value = pinned_scalar(def, kind);
            for (ci, done) in covered.iter().enumerate() {
                if *done {
                    continue;
                }
                let lit = self.logic.lit(id, ci as u32);
                let cond = self.logic.and(self.pc, lit);
                if cond.is_false() {
                    continue;
                }
                out.push(Variant::new(cond, value.clone()));
            }
        }
        out.normalize(&mut self.logic);
        if out.is_empty() {
            bail!(
                "option `{name}` is pinned by constraint, but none of its values apply \
                 where it is read here"
            );
        }
        Ok(out)
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
            ProjectOptionKind::Feature { value } => {
                Value::Obj(Obj::Feature(Rc::from(value.as_str())))
            }
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
        let (srcs, mut headers) = self.split_headers(srcs);

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

        self.list_headers(&include_dirs, &mut headers);

        let mut compile_args = Variational::empty();
        for key in ["c_args", "cpp_args", "objc_args", "objcpp_args", "args"] {
            if let Some(v) = args.get(key) {
                compile_args.extend(self.strings(v)?.map(|s| self.capture_flag(&s)));
            }
        }

        let mut link_args = Variational::empty();
        if let Some(v) = args.get("link_args") {
            link_args.extend(self.strings(v)?.map(|s| self.capture_flag(&s)));
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
            if let Some(v) = args.get(key)
                && let Ok(d) = self.deps(v)
            {
                deps.extend(d);
            }
        }

        let install = self.flag(args, "install", Pc::FALSE)?;
        let install_dir = self.opt_string(args, "install_dir")?.map(|v| v.to_string());
        let capture = self.opt_bool(args, "capture", false)?;

        let target = self.graph.target_mut(id);
        target.attrs.srcs = srcs;
        target.attrs.outs = outs;
        target.attrs.cmd = cmd;
        target.attrs.deps = deps;
        target.attrs.install = install;
        target.attrs.install_dir = install_dir;
        target.attrs.capture = capture;

        Ok(self.pure(Value::Obj(Obj::Target(id))))
    }

    /// `fs.copyfile(src, dst?)` — a custom target that copies one file into
    /// the build directory (meson stages a tool's data files this way). The
    /// destination name defaults to the source's basename.
    pub(crate) fn fn_fs_copyfile(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let src = args.at(0).ok_or_eyre("fs.copyfile() needs a source")?;
        let srcs = self.sources(src)?;

        let dst = match args.at(1) {
            Some(v) => self.one_string(v)?.to_string(),
            None => {
                let s = self.one_string(src)?;
                std::path::Path::new(&*s)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s.to_string())
            }
        };

        let dir = self.cur_dir().to_path_buf();
        let id = self.graph.add(&dst, &dir, self.pc, Kind::Custom);

        let mut cmd = Variational::empty();
        for arg in [
            CmdArg::Literal("cp".to_owned()),
            CmdArg::Inputs,
            CmdArg::Outputs,
        ] {
            cmd.push(Variant::new(self.pc, arg));
        }

        let install = self.flag(args, "install", Pc::FALSE)?;
        let install_dir = self.opt_string(args, "install_dir")?.map(|v| v.to_string());

        let target = self.graph.target_mut(id);
        target.attrs.srcs = srcs;
        target.attrs.outs = vec![dst];
        target.attrs.cmd = cmd;
        target.attrs.install = install;
        target.attrs.install_dir = install_dir;

        Ok(self.pure(Value::Obj(Obj::Target(id))))
    }

    fn fn_configure_file(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let output = self
            .opt_string(args, "output")?
            .ok_or_eyre("configure_file() needs an `output:`")?;

        // A `command:` makes this behave like `custom_target()`: an
        // arbitrary command produces the output, rather than a template or
        // `#define`s substituted from a `configuration:`.
        if let Some(command_arg) = args.get("command") {
            let dir = self.cur_dir().to_path_buf();
            let id = self.graph.add(&output, &dir, self.pc, Kind::Custom);

            let mut srcs = Variational::empty();
            if let Some(v) = args.get("input") {
                srcs.extend(self.sources(v)?);
            }
            let mut cmd = Variational::empty();
            cmd.extend(self.command(command_arg)?);

            let install = self.flag(args, "install", Pc::FALSE)?;
            let install_dir = self.opt_string(args, "install_dir")?.map(|v| v.to_string());

            let target = self.graph.target_mut(id);
            target.attrs.srcs = srcs;
            target.attrs.outs = vec![output.to_string()];
            target.attrs.cmd = cmd;
            target.attrs.install = install;
            target.attrs.install_dir = install_dir;

            return Ok(self.pure(Value::Obj(Obj::Target(id))));
        }

        let dir = self.cur_dir().to_path_buf();
        let id = self.graph.add(&output, &dir, self.pc, Kind::ConfigHeader);

        let template = match args.get("input") {
            Some(v) => self.sources(v)?.variants().first().map(|v| v.value.clone()),
            None => None,
        };

        let mut defines = match args.get("configuration") {
            Some(v) => self.defines(v)?,
            None => Variational::empty(),
        };

        // Meson substitutes every `#mesondefine` in a template, including a
        // name which configuration_data() never set.  Such a name is an
        // explicit `/* #undef NAME */`; without recording it here the backend
        // has no sed edit to make and leaves an invalid directive in the
        // generated header.  This especially matters when the only `.set()`
        // was in a branch made unreachable by a pinned option.
        if let Some(Source::File(path)) = &template
            && let Ok(text) = self.sources.read(&self.root.join(path))
        {
            let known: BTreeSet<String> = defines
                .variants()
                .iter()
                .map(|variant| variant.value.name.clone())
                .collect();
            for name in mesondefine_names(&text) {
                if !known.contains(&name) {
                    defines.push(Variant::new(
                        self.pc,
                        Define {
                            name,
                            value: DefineValue::Undef,
                        },
                    ));
                }
            }
        }

        // A `.pc` file is how a project tells the outside world about itself;
        // resolve it the way a real configure step would, straight from the
        // template and the configuration already computed above, so another
        // project's `dependency()` can read it back instead of decay.toml
        // having to repeat what is already known right here.
        if output.ends_with(".pc")
            && let Some(Source::File(path)) = &template
        {
            let variables = self.pc_variables(path, &defines);
            self.graph.provides.push(Package {
                name: output.trim_end_matches(".pc").to_owned(),
                target: None,
                variables,
            });
        }

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

    /// The `pkg-config` variables a `.pc` file would actually contain, found
    /// by substituting `defines` into `template` and reading the result back
    /// as pkg-config itself would, rather than guessing at what a configure
    /// step would have produced.
    fn pc_variables(
        &mut self,
        path: &Path,
        defines: &Variational<decay_build_ir::Define>,
    ) -> Vec<(String, String)> {
        let Ok(mut text) = self.sources.read(&self.root.join(path)) else {
            return Vec::new();
        };
        for (name, value) in single_valued(defines) {
            text = text.replace(&format!("@{name}@"), &value);
        }
        parse_pc_variables(&text)
    }

    /// The target for a build tool a gnome-module function runs itself,
    /// resolved the same way `find_program(name)` would be — in the project,
    /// mapped in the importer's configuration, or a hard failure, since a
    /// generator module always requires its tool.
    fn tool(&mut self, name: &str) -> eyre::Result<TargetId> {
        let tool_args = CallArgs {
            pos: vec![self.pure(Value::str(name))],
            kw: Vec::new(),
        };
        let found = self.fn_find_program(&tool_args)?;
        let Value::Obj(Obj::Program(program)) = &found.variants()[0].value else {
            unreachable!("find_program() always returns a Program")
        };
        Ok(program.target)
    }

    /// `gnome.compile_resources()`: a `glib-compile-resources` genrule.
    ///
    /// `input` is often itself generated (gtk4 builds most of its
    /// `.gresource.xml` files with a small Python script), so its resource
    /// list cannot be read at import time the way a real `.pc.in` template
    /// can. Rather than parse it, this hands the compiler the whole
    /// `source_dir` as a directory — a real one has to exist, since buck2
    /// otherwise has nothing to give it filesystem access to at all — and
    /// lets it resolve each referenced file itself, same as meson does.
    /// Anything meson would only have found in the build directory has to
    /// reach the compiler through `dependencies:` instead, which this does
    /// not yet wire in.
    pub(crate) fn fn_compile_resources(
        &mut self,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        let tool_target = self.tool("glib-compile-resources")?;

        let id = self.one_string(
            args.at(0)
                .ok_or_eyre("gnome.compile_resources() needs a name")?,
        )?;
        let input = args
            .at(1)
            .ok_or_eyre("gnome.compile_resources() needs an input")?;
        let resolved = self.sources(input)?;
        let Some(first) = resolved.variants().first() else {
            bail!("gnome.compile_resources('{id}') has no input");
        };
        let input_source = first.value.clone();

        // `meson.current_source_dir()` (the usual `source_dir:`) is already
        // project-relative; a plain string, the way meson reads any path, is
        // relative to wherever this call sits.
        let mut source_dirs = Vec::new();
        match args.get("source_dir") {
            Some(v) => {
                for variant in self.flat(v) {
                    match &variant.value {
                        Value::Obj(Obj::File(p)) => source_dirs.push(p.to_string()),
                        Value::Str(s) => source_dirs.push(self.resolve(s)),
                        other => bail!(
                            "`source_dir:` expects a string, found a {}",
                            other.type_name()
                        ),
                    }
                }
            }
            None => source_dirs.push(self.cur_dir().display().to_string()),
        }
        let real_dirs: Vec<PathBuf> = source_dirs
            .iter()
            .map(PathBuf::from)
            .filter(|d| self.sources.exists(&self.root.join(d)))
            .collect();
        if real_dirs.is_empty() {
            bail!(
                "gnome.compile_resources('{id}') names no `source_dir` that is a real \
                 directory in the project"
            );
        }

        let dir = self.cur_dir().to_path_buf();
        let target_id = self.graph.add(&id, &dir, self.pc, Kind::Custom);

        let mut srcs = Variational::empty();
        srcs.push(Variant::new(self.pc, input_source.clone()));

        let c_name = self
            .opt_string(args, "c_name")?
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                id.chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '_' })
                    .collect()
            });

        let mut cmd = Variational::empty();
        cmd.push(Variant::new(self.pc, CmdArg::Target(tool_target)));
        cmd.push(Variant::new(
            self.pc,
            CmdArg::Literal("--generate-source".to_owned()),
        ));
        for real_dir in real_dirs {
            cmd.push(Variant::new(
                self.pc,
                CmdArg::Literal("--sourcedir".to_owned()),
            ));
            cmd.push(Variant::new(self.pc, CmdArg::File(real_dir)));
        }
        cmd.push(Variant::new(
            self.pc,
            CmdArg::Literal(format!("--c-name={c_name}")),
        ));
        cmd.push(Variant::new(
            self.pc,
            CmdArg::Literal("--target".to_owned()),
        ));
        cmd.push(Variant::new(self.pc, CmdArg::Outputs));
        if let Some(v) = args.get("extra_args") {
            for s in self.strings(v)?.into_variants() {
                cmd.push(Variant::new(s.cond, CmdArg::Literal(s.value.to_string())));
            }
        }
        cmd.push(Variant::new(
            self.pc,
            match input_source {
                Source::File(path) => CmdArg::File(path),
                Source::Generated(gid) => CmdArg::Target(gid),
            },
        ));

        let target = self.graph.target_mut(target_id);
        target.attrs.srcs = srcs;
        target.attrs.outs = vec![format!("{id}.c")];
        target.attrs.cmd = cmd;

        Ok(self.pure(Value::list(vec![Variant::new(
            self.pc,
            Value::Obj(Obj::Target(target_id)),
        )])))
    }

    /// `gnome.mkenums()`: one `glib-mkenums` genrule per template given —
    /// meson runs the tool once per template file, scanning `sources:` for
    /// enum/flags declarations each time, and returns the results in the
    /// same order it was given the templates (`[c, h]`, conventionally).
    ///
    /// The templates meson bundles for the no-`*_template:` form are not
    /// reproduced here; a project that relies on them fails clearly instead
    /// of silently getting nothing to compile.
    pub(crate) fn fn_mkenums(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let tool_target = self.tool("glib-mkenums")?;

        let id = self.one_string(args.at(0).ok_or_eyre("gnome.mkenums() needs a name")?)?;

        let mut sources = Variational::empty();
        for arg in args.rest() {
            sources.extend(self.sources(arg)?);
        }
        if let Some(v) = args.get("sources") {
            sources.extend(self.sources(v)?);
        }

        let dir = self.cur_dir().to_path_buf();
        let mut outputs = Vec::new();
        for (key, ext) in [("c_template", "c"), ("h_template", "h")] {
            let Some(template_arg) = args.get(key) else {
                continue;
            };
            let templates = self.sources(template_arg)?;
            let Some(first) = templates.variants().first() else {
                continue;
            };
            let Source::File(template_path) = &first.value else {
                bail!("gnome.mkenums('{id}')'s `{key}:` must be a real file");
            };

            let cmd = [
                CmdArg::Literal("--template".to_owned()),
                CmdArg::File(template_path.clone()),
                CmdArg::Inputs,
                CmdArg::Literal(">".to_owned()),
                CmdArg::Outputs,
            ];
            let target_id =
                self.custom_run(&format!("{id}.{ext}"), &dir, tool_target, &sources, &cmd);
            outputs.push(Variant::new(self.pc, Value::Obj(Obj::Target(target_id))));
        }

        Ok(self.pure(Value::list(outputs)))
    }

    /// `gnome.mkenums_simple()`: `mkenums()` with meson's own bundled
    /// templates, reproduced here faithfully rather than approximated —
    /// this is boilerplate glib itself expects, not something this project
    /// gets to phrase differently.
    pub(crate) fn fn_mkenums_simple(
        &mut self,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        let tool_target = self.tool("glib-mkenums")?;
        let id = self.one_string(
            args.at(0)
                .ok_or_eyre("gnome.mkenums_simple() needs a name")?,
        )?;

        let mut sources = Variational::empty();
        if let Some(v) = args.get("sources") {
            sources.extend(self.sources(v)?);
        }

        let opt = |this: &mut Self, name| -> eyre::Result<String> {
            Ok(this
                .opt_string(args, name)?
                .map(|s| s.to_string())
                .unwrap_or_default())
        };
        let identifier_prefix = opt(self, "identifier_prefix")?;
        let symbol_prefix = opt(self, "symbol_prefix")?;
        let header_prefix = opt(self, "header_prefix")?;
        let function_prefix = opt(self, "function_prefix")?;
        let body_prefix = opt(self, "body_prefix")?;
        let decorator = opt(self, "decorator")?;

        let dir = self.cur_dir().to_path_buf();

        let mut common = Vec::new();
        if !identifier_prefix.is_empty() {
            common.push(CmdArg::Literal("--identifier-prefix".to_owned()));
            common.push(CmdArg::Literal(identifier_prefix));
        }
        if !symbol_prefix.is_empty() {
            common.push(CmdArg::Literal("--symbol-prefix".to_owned()));
            common.push(CmdArg::Literal(symbol_prefix));
        }

        // The headers' own `#include`s, relative to this directory, the way
        // meson's `os.path.relpath(hdr, subdir)` computes them.
        let mut includes = String::new();
        for variant in sources.variants() {
            if let Source::File(path) = &variant.value {
                let rel = path.strip_prefix(&dir).unwrap_or(path);
                includes.push_str(&format!("#include \"{}\"\n", rel.display()));
            }
        }

        let mut fhead = String::new();
        if !body_prefix.is_empty() {
            fhead.push_str(&body_prefix);
            fhead.push('\n');
        }
        fhead.push_str(&format!("#include \"{id}.h\"\n"));
        fhead.push_str(&includes);
        fhead.push_str("\n#define C_ENUM(v) ((gint) v)\n#define C_FLAGS(v) ((guint) v)\n");

        let mut c_cmd = common.clone();
        c_cmd.push(CmdArg::Literal("--fhead".to_owned()));
        c_cmd.push(CmdArg::Literal(fhead));
        c_cmd.push(CmdArg::Literal("--fprod".to_owned()));
        c_cmd.push(CmdArg::Literal(
            "\n/* enumerations from \"@basename@\" */\n".to_owned(),
        ));
        c_cmd.push(CmdArg::Literal("--vhead".to_owned()));
        c_cmd.push(CmdArg::Literal(format!(
            "\nGType\n{function_prefix}@enum_name@_get_type (void)\n{{\n    static gsize gtype_id = 0;\n    \
             static const G@Type@Value values[] = {{"
        )));
        c_cmd.push(CmdArg::Literal("--vprod".to_owned()));
        c_cmd.push(CmdArg::Literal(
            "        { C_@TYPE@ (@VALUENAME@), \"@VALUENAME@\", \"@valuenick@\" },".to_owned(),
        ));
        c_cmd.push(CmdArg::Literal("--vtail".to_owned()));
        c_cmd.push(CmdArg::Literal(
            "    { 0, NULL, NULL }\n    };\n    if (g_once_init_enter (&gtype_id)) {\n        \
             GType new_type = g_@type@_register_static (g_intern_static_string (\"@EnumName@\"), values);\n        \
             g_once_init_leave (&gtype_id, new_type);\n    }\n    return (GType) gtype_id;\n}"
                .to_owned(),
        ));
        c_cmd.push(CmdArg::Inputs);
        c_cmd.push(CmdArg::Literal(">".to_owned()));
        c_cmd.push(CmdArg::Outputs);
        let c_id = self.custom_run(&format!("{id}.c"), &dir, tool_target, &sources, &c_cmd);

        let mut header_prefix_line = header_prefix;
        if !header_prefix_line.is_empty() && !header_prefix_line.ends_with('\n') {
            header_prefix_line.push('\n');
        }
        let extra_newline = if decorator.is_empty() { "" } else { "\n" };

        let mut h_cmd = common;
        h_cmd.push(CmdArg::Literal("--fhead".to_owned()));
        h_cmd.push(CmdArg::Literal(format!(
            "#pragma once\n\n#include <glib-object.h>\n{header_prefix_line}\nG_BEGIN_DECLS\n"
        )));
        h_cmd.push(CmdArg::Literal("--fprod".to_owned()));
        h_cmd.push(CmdArg::Literal(
            "\n/* enumerations from \"@basename@\" */\n".to_owned(),
        ));
        h_cmd.push(CmdArg::Literal("--vhead".to_owned()));
        h_cmd.push(CmdArg::Literal(format!(
            "{extra_newline}{decorator}\nGType {function_prefix}@enum_name@_get_type (void);\n\
             #define @ENUMPREFIX@_TYPE_@ENUMSHORT@ ({function_prefix}@enum_name@_get_type())"
        )));
        h_cmd.push(CmdArg::Literal("--ftail".to_owned()));
        h_cmd.push(CmdArg::Literal("\nG_END_DECLS".to_owned()));
        h_cmd.push(CmdArg::Inputs);
        h_cmd.push(CmdArg::Literal(">".to_owned()));
        h_cmd.push(CmdArg::Outputs);
        let h_id = self.custom_run(&format!("{id}.h"), &dir, tool_target, &sources, &h_cmd);

        Ok(self.pure(Value::list(vec![
            Variant::new(self.pc, Value::Obj(Obj::Target(c_id))),
            Variant::new(self.pc, Value::Obj(Obj::Target(h_id))),
        ])))
    }

    /// `gnome.genmarshal()`: a `glib-genmarshal` header and body, one call
    /// each, both against the same `sources:` (a `.list` file naming the
    /// marshaller signatures — meson does not read it either; the tool
    /// does, at build time). Assumes a glib new enough for `--output` and
    /// `--include-header` (meson itself falls back to older flags below
    /// glib 2.51/2.53.4; this does not, since anything built against this
    /// importer is not going to be older than that).
    ///
    /// Returned as `[body, header]`, matching meson's own order.
    pub(crate) fn fn_genmarshal(&mut self, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let tool_target = self.tool("glib-genmarshal")?;
        let id = self.one_string(args.at(0).ok_or_eyre("gnome.genmarshal() needs a name")?)?;

        let mut sources = Variational::empty();
        if let Some(v) = args.get("sources") {
            sources.extend(self.sources(v)?);
        }
        for arg in args.rest() {
            sources.extend(self.sources(arg)?);
        }

        let dir = self.cur_dir().to_path_buf();

        let mut common = vec![CmdArg::Literal("--quiet".to_owned())];
        if let Some(prefix) = self.opt_string(args, "prefix")? {
            common.push(CmdArg::Literal("--prefix".to_owned()));
            common.push(CmdArg::Literal(prefix.to_string()));
        }
        if let Some(v) = args.get("extra_args") {
            for s in self.strings(v)?.into_variants() {
                common.push(CmdArg::Literal(s.value.to_string()));
            }
        }
        for flag in [
            "internal",
            "nostdinc",
            "skip_source",
            "stdinc",
            "valist_marshallers",
        ] {
            if self.flag(args, flag, Pc::FALSE)?.is_true() {
                common.push(CmdArg::Literal(format!("--{}", flag.replace('_', "-"))));
            }
        }
        common.push(CmdArg::Literal("--output".to_owned()));
        common.push(CmdArg::Outputs);

        let header_file = format!("{id}.h");
        let mut h_cmd = common.clone();
        h_cmd.push(CmdArg::Literal("--header".to_owned()));
        h_cmd.push(CmdArg::Inputs);
        h_cmd.push(CmdArg::Literal("--pragma-once".to_owned()));
        let h_id = self.custom_run(&header_file, &dir, tool_target, &sources, &h_cmd);

        let mut c_cmd = common;
        c_cmd.push(CmdArg::Literal("--body".to_owned()));
        c_cmd.push(CmdArg::Inputs);
        c_cmd.push(CmdArg::Literal("--include-header".to_owned()));
        c_cmd.push(CmdArg::Literal(header_file));
        let c_id = self.custom_run(&format!("{id}.c"), &dir, tool_target, &sources, &c_cmd);

        Ok(self.pure(Value::list(vec![
            Variant::new(self.pc, Value::Obj(Obj::Target(c_id))),
            Variant::new(self.pc, Value::Obj(Obj::Target(h_id))),
        ])))
    }

    /// One generator-tool invocation: `tool`, then `extra` verbatim (already
    /// in the exact argument order the caller wants, including however it
    /// names its own output — `--output @OUTPUT@`, or a trailing
    /// `> $OUT` for a tool that writes to stdout instead), against every
    /// source.
    fn custom_run(
        &mut self,
        output: &str,
        dir: &Path,
        tool: TargetId,
        sources: &Variational<Source>,
        extra: &[CmdArg],
    ) -> TargetId {
        let target_id = self.graph.add(output, dir, self.pc, Kind::Custom);

        let mut cmd = Variational::empty();
        cmd.push(Variant::new(self.pc, CmdArg::Target(tool)));
        for arg in extra {
            cmd.push(Variant::new(self.pc, arg.clone()));
        }

        let target = self.graph.target_mut(target_id);
        target.attrs.srcs = sources.clone();
        target.attrs.outs = vec![output.to_owned()];
        target.attrs.cmd = cmd;

        target_id
    }

    /// `import('pkgconfig').generate()`: record what the project makes
    /// available under the resulting `.pc` file's name, the same way
    /// `dependency()` would find it in another project.
    ///
    /// Meson also auto-fills `prefix`/`libdir`/`includedir`; only the
    /// `variables:` a project states explicitly are captured here, since
    /// those are what a project actually means to publish.
    pub(crate) fn fn_pkgconfig_generate(
        &mut self,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        let library = args.get("libraries").or_else(|| args.at(0));
        let library_target = match library {
            Some(v) => self.flat(v).into_iter().find_map(|v| match v.value {
                Value::Obj(Obj::Target(id)) => Some(id),
                _ => None,
            }),
            None => None,
        };

        let name = match self.opt_string(args, "filebase")? {
            Some(n) => n.to_string(),
            None => match self.opt_string(args, "name")? {
                Some(n) => n.to_string(),
                None => match library_target {
                    Some(id) => self.graph.target(id).label.clone(),
                    None => {
                        bail!("pkgconfig.generate() needs a `filebase:`, `name:`, or a library")
                    }
                },
            },
        };

        let variables = match args.get("variables") {
            Some(v) => single_valued_pairs(&self.pairs(v)?),
            None => Vec::new(),
        };

        self.graph.provides.push(Package {
            name,
            target: library_target,
            variables,
        });

        Ok(self.pure(Value::Unset))
    }

    /// Read a `configuration_data()` object, or a plain dict of the same
    /// shape, into header entries. Meson accepts either for
    /// `configure_file(configuration: ...)`; a dict's values are substituted
    /// exactly like `conf.set()` would (raw string/int, defined-or-undefined
    /// bool) since meson has no `set_quoted()` equivalent for a dict literal.
    fn defines(
        &mut self,
        v: &Variational<Value>,
    ) -> eyre::Result<Variational<decay_build_ir::Define>> {
        use decay_build_ir::{Define, DefineValue};

        let mut out = Variational::empty();
        for variant in v.variants() {
            match &variant.value {
                Value::Obj(Obj::ConfigData(data)) => {
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
                            out.push(Variant::new(
                                cond,
                                Define {
                                    name: name.to_string(),
                                    value,
                                },
                            ));
                        }
                    }
                }
                Value::Dict(entries) => {
                    for entry in entries.iter() {
                        let cond = self.logic.and(variant.cond, entry.cond);
                        if cond.is_false() {
                            continue;
                        }
                        let (name, value) = &entry.value;
                        let value = match value {
                            Value::Str(s) => DefineValue::Raw(s.to_string()),
                            Value::Int(n) => DefineValue::Number(*n),
                            Value::Bool(true) => DefineValue::Flag,
                            Value::Bool(false) => DefineValue::Undef,
                            other => bail!(
                                "`configuration:` dict values must be str, int, or bool, found a {}",
                                other.type_name()
                            ),
                        };
                        out.push(Variant::new(
                            cond,
                            Define {
                                name: name.to_string(),
                                value,
                            },
                        ));
                    }
                }
                other => bail!(
                    "`configuration:` expects configuration_data() or a dict, found a {}",
                    other.type_name()
                ),
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
            compile_args.extend(self.strings(v)?.map(|s| self.capture_flag(&s)));
        }

        let mut link_args = Variational::empty();
        if let Some(v) = args.get("link_args") {
            link_args.extend(self.strings(v)?.map(|s| self.capture_flag(&s)));
        }

        let mut headers = Variational::empty();
        if let Some(v) = args.get("sources") {
            headers.extend(self.sources(v)?);
        }
        self.list_headers(&include_dirs, &mut headers);

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
        target.attrs.variables = variables.clone();

        // A `declare_dependency()` in the project's own root `meson.build` is
        // that project's public face — the analogue of a wrap's
        // `[provide] dependency_names`. Recording it lets a sibling project
        // resolve `dependency('<this project>')` against it, the same way one
        // resolves against another's `pkg.generate()`. Subdir
        // `declare_dependency()` calls are internal glue and are left alone.
        if self.cur_dir().as_os_str().is_empty() {
            let provided = self.graph.project.name.clone();
            let variables = single_valued_pairs(&variables);
            self.graph.provides.push(Package {
                name: provided,
                target: Some(id),
                variables,
            });
        }

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
    pub(crate) fn pairs(
        &mut self,
        v: &Variational<Value>,
    ) -> eyre::Result<Variational<(String, String)>> {
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
                    out.push(Variant::new(
                        cond,
                        (entry.value.0.to_string(), value.to_string()),
                    ));
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

        // `dependency('threads')` is not a pkg-config module: meson resolves
        // it internally to the platform's threading support, and it is always
        // available. Emit a builtin target for it rather than an empty stub,
        // and never a "found" knob.
        if &*name == "threads" && self.oracle.dependency_found(&name).is_none() {
            let target = self.external("dep:threads", &name, External::Threads);
            let value = self.dep_obj(Dep {
                name: name.to_string(),
                found: self.pc,
                target,
                type_name: "threads",
                version: None,
                variables: Vec::new(),
            });
            return Ok(self.pure(value));
        }

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
        let variables = self.oracle.dependency_variables(&name);

        let value = self.dep_obj(Dep {
            name: name.to_string(),
            found,
            target,
            type_name,
            version: None,
            variables,
        });
        Ok(self.pure(value))
    }

    /// The condition under which a looked-up dependency is available.
    ///
    /// Where it was `required:`, a build that does not have it fails to
    /// configure at all — so rather than tracking a "found" flag that can never
    /// be false there, the configuration space is narrowed to say so.
    pub(crate) fn dependency_found(&mut self, key: &str, name: &str, required: Pc) -> Pc {
        if let Some(pinned) = self.oracle.dependency_found(name) {
            return Pc::from_bool(pinned);
        }
        let found = self.probe(key, format!("`{name}` is available"));
        if !required.is_false() {
            let must = self.logic.implies(required, found);
            self.logic.assume(must);
        }
        found
    }

    /// The `required:` argument as a condition.
    pub(crate) fn required(&mut self, args: &CallArgs) -> eyre::Result<Pc> {
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
        let value = self.resolve_program(&name, required)?;
        Ok(self.pure(value))
    }

    /// `import('python').find_installation()` — the interpreter that runs the
    /// build's Python tooling. It is looked up the same way any other program
    /// is (a `[programs]` entry, typically `python3`), and carries a fabricated
    /// language version, the way `meson.version()` and `cc.version()` are.
    pub(crate) fn fn_find_installation(
        &mut self,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        let name = match args.at(0) {
            Some(v) => self.one_string(v)?,
            None => Rc::from("python3"),
        };
        let required = self.required(args)?;
        let value = self.resolve_program(&name, required)?;
        Ok(self.pure(value))
    }

    /// `find_program('x')` proper: a program is found only when it is a script
    /// in the project tree or the importer was told where it lives; nothing a
    /// platform could set makes one appear, so a required one that is neither
    /// is a hard error and an optional one is simply absent.
    pub(crate) fn resolve_program(&mut self, name: &str, required: Pc) -> eyre::Result<Value> {
        // A program named by a path inside the project is a file, not something
        // to go looking for on the build machine.
        let candidate = self.resolve(name);
        let in_tree = self.sources.exists(&self.root.join(&candidate));
        let path = in_tree.then(|| PathBuf::from(&candidate));

        let key = format!("prog:{name}");
        let target = self.external(
            &key,
            name,
            External::Program {
                name: name.to_string(),
                path: path.clone(),
            },
        );

        let found = if in_tree || self.oracle.has_program(name) {
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

        Ok(self.program_obj(Program {
            name: name.to_string(),
            found,
            target,
            path: path.map(|p| p.display().to_string()),
        }))
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

    /// `windows.compile_resources('foo.rc', ...)` — compile a Windows
    /// resource script. buck2's C++ rules compile a `.rc` in `srcs` with the
    /// platform resource compiler, so each script is handed back as a source
    /// file and flows wherever the result is used. The `depend_files:` a
    /// script `#include`s ride along as sources too, so they are fetched.
    /// `args:` / `include_directories:` (resource-compiler flags and search
    /// paths) are not carried yet.
    pub(crate) fn fn_windows_compile_resources(
        &mut self,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        let mut items = Vec::new();
        for arg in args.pos.iter().chain(args.get("depend_files")) {
            for variant in self.flat(arg) {
                let value = match &variant.value {
                    Value::Str(s) => {
                        let path = self.resolve(s);
                        if !self.sources.exists(&self.root.join(&path)) {
                            warn!(%path, "windows.compile_resources() names a path that is not in the source tree");
                        }
                        Value::Obj(Obj::File(Rc::from(path.as_str())))
                    }
                    // A `.rc` produced by `configure_file()` or another target
                    // rides through as that target's output.
                    Value::Obj(Obj::File(_) | Obj::Target(_) | Obj::Output(..)) => {
                        variant.value.clone()
                    }
                    other => bail!(
                        "windows.compile_resources() cannot take a {}",
                        other.type_name()
                    ),
                };
                items.push(Variant::new(variant.cond, value));
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
                    next.push(Variant::new(
                        cond,
                        join_paths([base.value.as_str(), &part.value]),
                    ));
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

        // `args:` can hold a build target directly (libglvnd's symbol-check
        // tests hand a just-built shared_library() to a checker script), not
        // just strings — the same shape a `custom_target()` command already
        // reads, so reuse it here too.
        let mut cmd_args = Variational::empty();
        if let Some(v) = args.get("args") {
            cmd_args.extend(self.command(v)?);
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

    fn fn_install(
        &mut self,
        args: &CallArgs,
        default_dir: &str,
    ) -> eyre::Result<Variational<Value>> {
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
    /// The positional string(s) — one call may pass either `'a', 'b'` or
    /// `['a', 'b']` — of an `add_*_arguments()` call.
    fn flag_args(&mut self, args: &CallArgs) -> eyre::Result<Variational<String>> {
        let mut out = Variational::empty();
        for arg in &args.pos {
            out.extend(self.strings(arg)?.map(|s| s.to_string()));
        }
        Ok(out)
    }

    /// Add every header sitting in `include_dirs` that is not already
    /// explicitly listed in `headers`.
    ///
    /// Meson gives a compiled file free quote-include access to anything
    /// under one of its `include_directories()`, because `-I` is real
    /// filesystem access; buck2's sandbox is not, so a header sitting there
    /// has to actually be listed for a compile to find it, the same way an
    /// explicit one already is.
    fn list_headers(
        &mut self,
        include_dirs: &Variational<PathBuf>,
        headers: &mut Variational<Source>,
    ) {
        for variant in include_dirs.variants().to_vec() {
            for rel in self.sources.list_dir(&self.root.join(&variant.value)) {
                if !is_header_file(&rel) {
                    continue;
                }
                let path = variant.value.join(&rel);
                let already_listed = headers
                    .variants()
                    .iter()
                    .any(|h| matches!(&h.value, Source::File(p) if *p == path));
                if !already_listed {
                    headers.push(Variant::new(variant.cond, Source::File(path)));
                }
            }
        }
    }

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
            if is_header_file(&name) {
                headers.push(variant);
            } else {
                compiled.push(variant);
            }
        }
        (compiled, headers)
    }

    /// A boolean keyword argument, as a condition.
    pub(crate) fn flag(&mut self, args: &CallArgs, name: &str, absent: Pc) -> eyre::Result<Pc> {
        let Some(v) = args.get(name) else {
            return Ok(absent);
        };
        let t = self.truth(v)?;
        Ok(self.logic.and(self.pc, t))
    }
}

/// The names in valid `#mesondefine NAME` template directives.
///
/// Meson permits indentation before the directive and trailing text after the
/// name.  A directive name follows the C preprocessor identifier grammar;
/// rejecting other lines avoids mistaking a comment or prose for a define.
fn mesondefine_names(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("#mesondefine")?;
            rest.chars().next().filter(|c| c.is_whitespace())?;
            let name = rest.split_whitespace().next()?;
            let mut chars = name.chars();
            let first = chars.next()?;
            (first == '_' || first.is_ascii_alphabetic())
                .then_some(())
                .filter(|_| chars.all(|c| c == '_' || c.is_ascii_alphanumeric()))?;
            Some(name.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::mesondefine_names;
    use std::collections::BTreeSet;

    #[test]
    fn finds_only_valid_mesondefine_directives() {
        let names = mesondefine_names(
            "\n  #mesondefine ENABLE_FEATURE\n#mesondefine _PRIVATE 1\n\
             #mesondefine 1INVALID\n#mesondefine ALSO-INVALID\n#mesondefineNOT_A_DIRECTIVE\n\
             # mesondefine COMMENT\n",
        );
        assert_eq!(
            names,
            BTreeSet::from(["ENABLE_FEATURE".to_owned(), "_PRIVATE".to_owned()])
        );
    }
}

/// Every `#define` whose value does not depend on the configuration, as the
/// plain text it would substitute into a `.in` template — the same rule
/// `decay_buck2` renders a template substitution with, so resolving one here
/// at import time agrees with what the generated build would produce.
///
/// A name with more than one variant genuinely depends on a configuration
/// choice still open at import time, so there is no one answer to give; it is
/// left out rather than guessed at.
fn single_valued(defines: &Variational<decay_build_ir::Define>) -> Vec<(String, String)> {
    use {decay_build_ir::DefineValue, std::collections::HashMap};

    let mut counts: HashMap<&str, u32> = HashMap::new();
    for variant in defines.variants() {
        *counts.entry(variant.value.name.as_str()).or_default() += 1;
    }

    defines
        .variants()
        .iter()
        .filter(|v| counts[v.value.name.as_str()] == 1)
        .map(|v| {
            let text = match &v.value.value {
                DefineValue::Quoted(s) | DefineValue::Raw(s) => s.clone(),
                DefineValue::Number(n) => n.to_string(),
                DefineValue::Flag => "1".to_owned(),
                DefineValue::Undef => String::new(),
            };
            (v.value.name.clone(), text)
        })
        .collect()
}

/// Keep only the `variables:` entries whose value does not depend on the
/// configuration, for the same reason [`single_valued`] does: a name with
/// more than one variant has no one answer to give yet.
fn single_valued_pairs(pairs: &Variational<(String, String)>) -> Vec<(String, String)> {
    use std::collections::HashMap;

    let mut counts: HashMap<&str, u32> = HashMap::new();
    for variant in pairs.variants() {
        *counts.entry(variant.value.0.as_str()).or_default() += 1;
    }

    pairs
        .variants()
        .iter()
        .filter(|v| counts[v.value.0.as_str()] == 1)
        .map(|v| v.value.clone())
        .collect()
}

/// The `name=value` variable lines of a `pkg-config` file, e.g. `prefix=/usr`
/// ahead of the `Name:`/`Version:`/... fields. A line belongs to whichever
/// syntax its first `=` or `:` matches, the same way pkg-config itself reads
/// one.
fn parse_pc_variables(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let eq = line.find('=');
        let colon = line.find(':');
        let is_variable = match (eq, colon) {
            (Some(e), Some(c)) => e < c,
            (Some(_), None) => true,
            _ => false,
        };
        if let (true, Some((k, v))) = (is_variable, line.split_once('=')) {
            out.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    out
}

/// Whether a source-tree path is a header rather than something compiled on
/// its own.
fn is_header_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("h" | "hh" | "hpp" | "hxx" | "inc" | "def")
    )
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
        // `ByConstraint` never reaches here: `fn_get_option` peels it off
        // into `option_by_constraint` before any scalar conversion.
        Pinned::ByConstraint { .. } => unreachable!("by-constraint pin is not a scalar"),
    }
}

/// A pinned scalar as the [`Value`] `get_option` returns, coerced to the
/// option's declared kind so a `feature` string comes back as a feature
/// object. A no-op for string/combo options, and when the kind is unknown.
fn pinned_scalar(pinned: &Pinned, kind: Option<&ProjectOptionKind>) -> Value {
    match (pinned, kind) {
        (Pinned::Str(s), Some(kind)) => option_choice(kind, s),
        _ => pinned_value(pinned),
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
            &[
                "plain",
                "debug",
                "debugoptimized",
                "release",
                "minsize",
                "custom",
            ],
            "debug",
        ),
        "optimization" => combo(&["plain", "0", "g", "1", "2", "3", "s"], "0"),
        "warning_level" => combo(&["0", "1", "2", "3", "everything"], "1"),
        "b_ndebug" => combo(&["true", "false", "if-release"], "false"),
        "b_vscrt" => combo(
            &["none", "md", "mdd", "mt", "mtd", "from_buildtype"],
            "from_buildtype",
        ),
        "b_sanitize" => combo(
            &[
                "none",
                "address",
                "thread",
                "undefined",
                "memory",
                "leak",
                "address,undefined",
            ],
            "none",
        ),
        "b_pgo" => combo(&["off", "generate", "use"], "off"),
        "b_colorout" => combo(&["auto", "always", "never"], "always"),
        "layout" => combo(&["mirror", "flat"], "mirror"),
        "wrap_mode" => combo(
            &[
                "default",
                "nofallback",
                "nodownload",
                "forcefallback",
                "nopromote",
            ],
            "default",
        ),
        "backend" => combo(&["ninja", "vs", "xcode", "none"], "ninja"),
        "c_std" | "cpp_std" | "objc_std" | "objcpp_std" => combo(
            &[
                "none", "c89", "c99", "c11", "c17", "c18", "c2x", "gnu89", "gnu99", "gnu11",
                "gnu17", "gnu18", "gnu2x", "c++98", "c++11", "c++14", "c++17", "c++20", "gnu++11",
                "gnu++14", "gnu++17", "gnu++20",
            ],
            "none",
        ),
        "unity" => combo(&["on", "off", "subprojects"], "off"),

        "debug" => bool_opt(true),
        "strip" | "werror" | "prefer_static" | "b_lto" | "b_coverage" | "b_pie" | "vsenv" => {
            bool_opt(false)
        }
        "b_staticpic" | "b_asneeded" | "b_lundef" | "b_pch" => bool_opt(true),
        "b_bitcode" => bool_opt(false),

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
