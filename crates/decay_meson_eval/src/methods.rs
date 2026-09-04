use {
    crate::{
        Interp,
        args::CallArgs,
        obj::{
            Dep,
            Entry,
            Lang,
            Machine,
            Module,
            Obj, //
        },
        oracle::{
            Probe,
            SizeAnswer,
            SizeQuery, //
        },
        val::Value,
    },
    decay_build_ir::External,
    decay_meson_ast::Loc,
    decay_meson_logic::{
        ANY_OTHER,
        Pc,
        Solver,
        Var,
        VarId,
        VarKind,
        Variant,
        Variational, //
    },
    eyre::{
        OptionExt,
        bail, //
    },
    std::{
        rc::Rc,
        str::FromStr, //
    },
    tracing::debug,
};

impl<'a, S: Solver> Interp<'a, S> {
    pub(crate) fn method(
        &mut self,
        obj: &Variational<Value>,
        name: &str,
        args: &CallArgs,
        loc: Loc,
    ) -> eyre::Result<Variational<Value>> {
        // The receiver may itself differ between configurations, so dispatch
        // happens per variant and the results are unioned back together.
        let mut out = Variational::empty();
        for variant in obj.variants().to_vec() {
            let cond = self.logic.and(self.pc, variant.cond);
            if cond.is_false() {
                continue;
            }
            let result = self.with_pc(cond, |this| this.method1(&variant.value, name, args, loc))?;
            out.extend(result.restrict(&mut self.logic, cond));
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }

    fn method1(
        &mut self,
        obj: &Value,
        name: &str,
        args: &CallArgs,
        loc: Loc,
    ) -> eyre::Result<Variational<Value>> {
        match obj {
            Value::Obj(o) => self.obj_method(o, name, args, loc),
            Value::Str(s) => self.str_method(s, name, args),
            // A deferred concatenation the current path pins down to one string
            // behaves like any other string; one that genuinely still varies
            // has no single value to call a method on.
            Value::StrCat(pieces) => {
                let pc = self.pc;
                if pieces.iter().all(|p| p.cond.is_true() || self.logic.entails(pc, p.cond)) {
                    let s: Rc<str> = pieces.iter().map(|p| &*p.value).collect::<String>().into();
                    self.str_method(&s, name, args)
                } else {
                    bail!(
                        "`.{name}()` on a string whose value depends on the configuration \
                         has no single answer"
                    )
                }
            }
            Value::Int(i) => self.int_method(*i, name, args),
            Value::Bool(b) => self.bool_method(*b, name, args),
            Value::List(_) => self.list_method(obj, name, args),
            Value::Dict(_) => self.dict_method(obj, name, args),
            other => bail!("a {} has no method `{name}`", other.type_name()),
        }
    }

    // -- objects ----------------------------------------------------------

    fn obj_method(
        &mut self,
        obj: &Obj,
        name: &str,
        args: &CallArgs,
        loc: Loc,
    ) -> eyre::Result<Variational<Value>> {
        match (obj, name) {
            // -- meson --
            (Obj::Meson, "project_name") => {
                let v = self.graph.project.name.clone();
                Ok(self.pure(Value::from(v)))
            }
            (Obj::Meson, "project_version") => {
                let v = self
                    .graph
                    .project
                    .version
                    .clone()
                    .ok_or_eyre("project() declared no version")?;
                Ok(self.pure(Value::from(v)))
            }
            (Obj::Meson, "project_license") => {
                let pc = self.pc;
                let items = self
                    .graph
                    .project
                    .license
                    .clone()
                    .into_iter()
                    .map(|l| Variant::new(pc, Value::from(l)))
                    .collect();
                Ok(self.pure(Value::list(items)))
            }
            (Obj::Meson, "version") => Ok(self.pure(Value::str("1.10.0"))),
            (Obj::Meson, "backend") => Ok(self.pure(Value::str("ninja"))),
            (Obj::Meson, "is_subproject") => Ok(self.bool_value(Pc::FALSE)),
            (Obj::Meson, "is_cross_build") => Ok(self.bool_value(Pc::FALSE)),
            (Obj::Meson, "can_run_host_binaries") => Ok(self.bool_value(self.pc)),
            (Obj::Meson, "get_compiler") => {
                let lang = self.one_string(args.at(0).ok_or_eyre("expected a language")?)?;
                let lang = Lang::from_str(&lang)?;
                Ok(self.pure(Value::Obj(Obj::Compiler(lang))))
            }
            // A path in the source tree, not a plain string: a project that
            // joins one with `/` and hands the result to a command, as
            // iso-codes does to find its own `data/`, means the fetched
            // checkout, and `command()` has to turn it back into a reference
            // to that checkout rather than a path that means nothing once the
            // build runs somewhere else.
            (
                Obj::Meson,
                "source_root" | "project_source_root" | "global_source_root" | "current_source_dir",
            ) => {
                let dir = if name == "current_source_dir" {
                    self.cur_dir().display().to_string()
                } else {
                    String::new()
                };
                Ok(self.pure(Value::Obj(Obj::File(Rc::from(dir.as_str())))))
            }
            (
                Obj::Meson,
                "build_root" | "project_build_root" | "global_build_root" | "current_build_dir",
            ) => Ok(self.pure(Value::str("."))),
            (
                Obj::Meson,
                "add_install_script"
                | "add_dist_script"
                | "add_postconf_script"
                | "install_dependency_manifest"
                | "override_find_program"
                | "override_dependency",
            ) => {
                self.warn_unsupported(&format!("`meson.{name}()`"), loc);
                Ok(self.pure(Value::Unset))
            }

            // -- machines --
            (Obj::Machine(machine), _) => self.machine_property(*machine, name),

            // -- compilers --
            (Obj::Compiler(lang), _) => self.compiler_method(*lang, name, args),

            // -- configuration data --
            (Obj::ConfigData(data), _) => {
                let data = data.clone();
                self.config_method(&data, name, args)
            }

            // -- dependencies --
            (Obj::Dep(dep), "found") => Ok(self.bool_value(dep.found)),
            // The one method a disabler actually defines: always absent,
            // wherever it stands in for a dependency or a program.
            (Obj::Disabler, "found") => Ok(self.bool_value(Pc::FALSE)),
            (Obj::Dep(dep), "type_name") => Ok(self.pure(Value::str(dep.type_name))),
            (Obj::Dep(dep), "name") => {
                let v = dep.name.clone();
                Ok(self.pure(Value::from(v)))
            }
            (Obj::Dep(dep), "version") => {
                let v = dep.version.clone().unwrap_or_else(|| "unknown".to_owned());
                Ok(self.pure(Value::from(v)))
            }
            (Obj::Dep(dep), "partial_dependency" | "as_system" | "as_link_whole") => {
                // A partial dependency drops some usage requirements. Nothing
                // downstream reads them separately yet, so the whole dependency
                // is handed back rather than silently losing the edge.
                Ok(self.pure(Value::Obj(Obj::Dep(dep.clone()))))
            }
            (Obj::Dep(dep), "get_variable" | "get_pkgconfig_variable") => {
                let key = self.opt_string(args, "pkgconfig")?;
                let key = match key {
                    Some(k) => Some(k),
                    None => match args.at(0) {
                        Some(v) => Some(self.one_string(v)?),
                        None => None,
                    },
                };

                // An availability flag the importer answers from a constraint
                // stays a real build-time choice — e.g. whether SSE2 is
                // available follows from the CPU — instead of collapsing to
                // whatever the dependency's own default build happened to be.
                if let Some(k) = &key
                    && let Some(probe) = self.oracle.dependency_variable(&dep.name, k)
                {
                    let description = format!("`{k}` of `{}` is set", dep.name);
                    let cond =
                        self.resolve_probe(Some(probe), &format!("dep_var:{}:{k}", dep.name), description)?;
                    return Ok(self.flag_value(cond));
                }

                let found = key
                    .and_then(|k| {
                        dep.variables
                            .iter()
                            .find(|(name, _)| name.as_str() == &*k)
                            .map(|(_, v)| v.clone())
                    })
                    .or_else(|| self.default_string(args));
                match found {
                    Some(v) => Ok(self.pure(Value::from(v))),
                    None => bail!("`{}` has no such variable", dep.name),
                }
            }

            // -- programs --
            (Obj::Program(program), "found") => Ok(self.bool_value(program.found)),
            (Obj::Program(program), "path" | "full_path") => {
                let v = program
                    .path
                    .clone()
                    .unwrap_or_else(|| program.name.clone());
                Ok(self.pure(Value::from(v)))
            }
            (Obj::Program(program), "version") => {
                let _ = program;
                Ok(self.pure(Value::str("unknown")))
            }

            // -- build targets --
            (Obj::Target(id), "full_path" | "path") => {
                let v = self.graph.target(*id).name.clone();
                Ok(self.pure(Value::from(v)))
            }
            (Obj::Target(id), "name") => {
                let v = self.graph.target(*id).label.clone();
                Ok(self.pure(Value::from(v)))
            }
            (Obj::Target(id), "extract_all_objects" | "extract_objects") => {
                Ok(self.pure(Value::Obj(Obj::Target(*id))))
            }
            (Obj::Target(_), "private_dir_include") => {
                Ok(self.pure(Value::Obj(Obj::IncludeDirs(Rc::new(Vec::new())))))
            }

            // -- modules --
            (Obj::Module(Module::PkgConfig), "generate") => self.fn_pkgconfig_generate(args),
            (Obj::Module(Module::GNOME), "compile_resources") => self.fn_compile_resources(args),
            (Obj::Module(Module::GNOME), "mkenums") => self.fn_mkenums(args),
            (Obj::Module(Module::GNOME), "mkenums_simple") => self.fn_mkenums_simple(args),
            (Obj::Module(Module::GNOME), "genmarshal") => self.fn_genmarshal(args),
            (Obj::Module(Module::Fs), "exists" | "is_file" | "is_dir") => {
                let path = self.one_string(args.at(0).ok_or_eyre("expected a path")?)?;
                let resolved = self.resolve(&path);
                let exists = self.sources.exists(&self.root.join(&resolved));
                Ok(self.bool_value(if exists { self.pc } else { Pc::FALSE }))
            }
            (Obj::Module(Module::Fs), "read") => {
                let path = self.one_string(args.at(0).ok_or_eyre("expected a path")?)?;
                let resolved = self.resolve(&path);
                let content = self.sources.read(&self.root.join(&resolved))?;
                Ok(self.pure(Value::from(content)))
            }
            (Obj::Module(Module::I18n), "gettext") => {
                // A ninja build compiles `.mo` files only at `meson install`
                // time, not as part of the normal build; there is nothing here
                // for a build graph without an install step to depend on.
                debug!("i18n.gettext() has no build-graph equivalent; skipping");
                Ok(self.pure(Value::Unset))
            }
            (Obj::Module(m), _) => bail!("module `{m:?}` has no method `{name}`"),

            // -- features --
            (Obj::Feature(f), "enabled") => {
                Ok(self.bool_value(if &**f == "enabled" { self.pc } else { Pc::FALSE }))
            }
            (Obj::Feature(f), "disabled") => {
                Ok(self.bool_value(if &**f == "disabled" { self.pc } else { Pc::FALSE }))
            }
            (Obj::Feature(f), "auto") => {
                Ok(self.bool_value(if &**f == "auto" { self.pc } else { Pc::FALSE }))
            }
            (Obj::Feature(f), "allowed") => {
                Ok(self.bool_value(if &**f == "disabled" { Pc::FALSE } else { self.pc }))
            }
            (Obj::Feature(f), "require" | "disable_auto_if" | "enable_auto_if") => {
                Ok(self.pure(Value::Obj(Obj::Feature(f.clone()))))
            }

            (Obj::File(path), "full_path") => {
                let v = path.to_string();
                Ok(self.pure(Value::from(v)))
            }

            (obj, name) => bail!("a {} has no method `{name}`", obj.type_name()),
        }
    }

    fn default_string(&mut self, args: &CallArgs) -> Option<String> {
        args.get("default_value")
            .and_then(|v| self.one_string(v).ok())
            .map(|v| v.to_string())
    }

    // -- machine properties -----------------------------------------------

    fn machine_property(
        &mut self,
        machine: Machine,
        property: &str,
    ) -> eyre::Result<Variational<Value>> {
        // A cross file can set `subsystem` apart from `system` (say, `ios`
        // under `darwin`); nothing here models cross files at all, so the
        // two are answered as the very same variable, exactly like a
        // native build where nobody set one differently.
        let property = if property == "subsystem" { "system" } else { property };

        if let Some(pinned) = self.oracle.machine(machine, property) {
            return Ok(self.pure(Value::from(pinned)));
        }

        let choices = match property {
            "system" => self.oracle.systems(),
            "endian" => ["little", "big"].map(str::to_owned).to_vec(),
            "cpu_family" => [
                "x86", "x86_64", "arm", "aarch64", "riscv32", "riscv64", "ppc64", "s390x",
                "mips64", "loongarch64", "wasm32",
            ]
            .map(str::to_owned)
            .to_vec(),
            "cpu" => bail!(
                "`{}_machine.cpu()` names an exact CPU, which cannot be left open; \
                 pin it in the importer configuration",
                machine.as_str()
            ),
            other => bail!("unknown machine property `{other}()`"),
        };

        if choices.is_empty() {
            bail!(
                "`{}_machine.{property}()` was left open but no candidates were configured",
                machine.as_str()
            );
        }
        if choices.len() == 1 {
            return Ok(self.pure(Value::from(choices[0].clone())));
        }

        let id = self.machine_var(machine, property, choices.clone());

        let mut out = Variational::empty();
        for (i, choice) in choices.iter().enumerate() {
            let lit = self.logic.lit(id, i as u32);
            let cond = self.logic.and(self.pc, lit);
            if cond.is_false() {
                continue;
            }
            out.push(Variant::new(cond, Value::from(choice.clone())));
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }

    /// The variable standing for a machine property left open.
    fn machine_var(&mut self, machine: Machine, property: &str, choices: Vec<String>) -> VarId {
        self.logic.declare(Var {
            key: format!("machine:{}:{property}", machine.as_str()),
            description: Some(format!("{} machine {property}", machine.as_str())),
            kind: VarKind::Machine,
            choices,
            default: 0,
        })
    }

    /// The condition for the code being compiled for one of `systems`.
    ///
    /// This is the host machine, the one meson compiles for. A build that was
    /// told which system it targets gets a plain answer and no variable at all.
    fn host_system_is(&mut self, systems: &[String], probe: &str) -> eyre::Result<Pc> {
        let known = self.oracle.systems();
        for system in systems {
            if !known.iter().any(|k| k == system) {
                bail!(
                    "`{probe}` is configured for system `{system}`, which is not one of \
                     the systems the importer was configured with"
                );
            }
        }

        if let Some(pinned) = self.oracle.machine(Machine::Host, "system") {
            return Ok(Pc::from_bool(systems.iter().any(|s| *s == pinned)));
        }
        match known.len() {
            0 => bail!("`{probe}` answers by system but no systems were configured"),
            1 => return Ok(Pc::from_bool(systems.contains(&known[0]))),
            _ => {}
        }

        let id = self.machine_var(Machine::Host, "system", known.clone());
        let choices = known
            .iter()
            .enumerate()
            .filter(|(_, k)| systems.iter().any(|s| s == *k))
            .map(|(i, _)| i as u32);
        Ok(self.logic.any_of(id, choices))
    }

    /// The condition for a foreign constraint holding one of `values`.
    ///
    /// The importer did not declare this constraint and cannot know every value
    /// it has, so the values the configuration named become choices and
    /// everything else is one more.
    fn constraint_is(&mut self, setting: &str, domain: Vec<String>, values: &[String]) -> Pc {
        let (id, choices) = self.constraint_var(setting, domain);
        let holds = choices
            .iter()
            .enumerate()
            .filter(|(_, c)| values.iter().any(|v| v == *c))
            .map(|(i, _)| i as u32);
        self.logic.any_of(id, holds)
    }

    /// The variable standing for a constraint the importer did not declare,
    /// with `ANY_OTHER` appended for the values it cannot name. Declaring is
    /// keyed by `setting`, so every caller — `[probes]`, `[sizeof]`,
    /// `[alignment]` — shares one variable per constraint and its `select()`s
    /// all key on the same thing. Returns the choice list too, so a caller can
    /// map a value back to its index.
    fn constraint_var(&mut self, setting: &str, domain: Vec<String>) -> (VarId, Vec<String>) {
        let mut choices = domain;
        choices.push(ANY_OTHER.to_owned());
        let id = self.logic.declare(Var {
            key: format!("constraint:{setting}"),
            description: Some(format!("the `{setting}` constraint")),
            kind: VarKind::Constraint,
            // Nothing else is what a build that says nothing gets, so it is the
            // value the `select()` falls back to.
            default: choices.len() - 1,
            choices: choices.clone(),
        });
        (id, choices)
    }

    /// `cc.sizeof()` / `cc.alignment()`: a target-dependent integer the
    /// configuration has to pin, since the importer cannot run the compiler
    /// and the number is baked into a generated header.
    fn type_size(&mut self, query: SizeQuery, args: &CallArgs) -> eyre::Result<Variational<Value>> {
        let ty = self.one_string(
            args.at(0)
                .ok_or_eyre("`sizeof()` / `alignment()` needs a type name")?,
        )?;
        match self.oracle.type_size(query, &ty) {
            Some(SizeAnswer::Fixed(n)) => Ok(self.pure(Value::Int(n))),
            Some(SizeAnswer::Constraint {
                setting,
                domain,
                cases,
            }) => {
                let (id, choices) = self.constraint_var(&setting, domain);
                let mut out = Variational::empty();
                for (value, size) in cases {
                    let Some(choice) = choices.iter().position(|c| *c == value) else {
                        continue;
                    };
                    let lit = self.logic.lit(id, choice as u32);
                    let cond = self.logic.and(self.pc, lit);
                    if cond.is_false() {
                        continue;
                    }
                    out.push(Variant::new(cond, Value::Int(size)));
                }
                out.normalize(&mut self.logic);
                if out.is_empty() {
                    bail!(
                        "`{}('{ty}')` is configured, but none of its constraint values \
                         apply where it is used here",
                        query.as_str()
                    );
                }
                Ok(out)
            }
            None => bail!(
                "`{}('{ty}')` yields a target-dependent number that cannot be left open; \
                 add `{}.\"{ty}\"` to decay.toml (a single integer, or a table of \
                 constraint value to integer)",
                query.as_str(),
                query.as_str(),
            ),
        }
    }

    /// The condition for a compiler probe having succeeded.
    ///
    /// Most probes become a knob of their own, because the importer cannot run
    /// the compiler. One the configuration ties to the operating system asks
    /// the machine instead, so the generated build selects on the system it
    /// already knows rather than on a second, redundant constraint.
    fn probe_cond(&mut self, lang: Lang, name: &str, what: &str) -> eyre::Result<Pc> {
        let answer = self.oracle.probe(name, what);
        let key = format!("probe:{}:{name}:{what}", lang.as_str());
        let description = format!("`{what}` is available to the {} compiler", lang.as_str());
        self.resolve_probe(answer, &key, description)
    }

    /// Turn an [`Oracle`] answer into a condition, the way [`Self::probe_cond`]
    /// does, but for anything else the importer answers the same way — a
    /// dependency's `pkg-config` flag, say. `key`/`description` name the
    /// fresh variable declared when the importer says nothing at all.
    fn resolve_probe(&mut self, answer: Option<Probe>, key: &str, description: String) -> eyre::Result<Pc> {
        match answer {
            Some(Probe::Fixed(answer)) => Ok(Pc::from_bool(answer)),
            Some(Probe::Systems(systems)) => self.host_system_is(&systems, key),
            Some(Probe::Constraint {
                setting,
                domain,
                values,
            }) => Ok(self.constraint_is(&setting, domain, &values)),
            None => Ok(self.probe(key, description)),
        }
    }

    // -- compilers --------------------------------------------------------

    fn compiler_method(
        &mut self,
        lang: Lang,
        name: &str,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        match name {
            "get_id" | "get_linker_id" => self.compiler_id(lang),
            "get_argument_syntax" => self.compiler_id(lang),
            "version" => Ok(self.pure(Value::str("0"))),
            "cmd_array" => {
                let pc = self.pc;
                Ok(self.pure(Value::list(vec![Variant::new(
                    pc,
                    Value::str(lang.as_str()),
                )])))
            }

            // Probes: the importer cannot compile anything, so each answer
            // becomes a configuration knob the build can be told about.
            "has_header" | "check_header" | "has_function" | "has_type" | "has_member"
            | "has_header_symbol" | "symbols_have_underscore_prefix" => {
                let what = match args.at(0) {
                    Some(v) => self.one_string(v).unwrap_or_else(|_| Rc::from("expr")),
                    None => Rc::from("expr"),
                };
                let cond = self.probe_cond(lang, name, &what)?;
                Ok(self.bool_value(cond))
            }
            // A source snippet makes an unwieldy, fragile probe key — and, if
            // it spans several lines, an invalid one to write into a
            // generated file at all. `name:` is exactly what meson has these
            // take for identifying the check to a person; prefer it, falling
            // back to the snippet only when the project gave no name.
            "compiles" | "links" | "run" => {
                let what = match self.opt_string(args, "name")? {
                    Some(n) => n,
                    None => match args.at(0) {
                        Some(v) => self.one_string(v).unwrap_or_else(|_| Rc::from("expr")),
                        None => Rc::from("expr"),
                    },
                };
                let cond = self.probe_cond(lang, name, &what)?;
                Ok(self.bool_value(cond))
            }
            "sizeof" => self.type_size(SizeQuery::Sizeof, args),
            "alignment" => self.type_size(SizeQuery::Alignment, args),
            // `compute_int` evaluates a constant expression by compiling; the
            // importer cannot. Meson itself falls back to `guess:` when it
            // cannot run the result (a cross build), so do the same — a
            // project that passes a guess has already said what to assume.
            "compute_int" => {
                let expr = self
                    .one_string(args.at(0).ok_or_eyre("`compute_int()` needs an expression")?)
                    .unwrap_or_else(|_| Rc::from("expr"));
                match args.get("guess") {
                    Some(g) => {
                        let n = self.one_int(g)?;
                        Ok(self.pure(Value::Int(n)))
                    }
                    None => bail!(
                        "`compute_int('{expr}')` computes a target-dependent number the \
                         importer cannot evaluate, and the project gave no `guess:` to \
                         fall back on"
                    ),
                }
            }
            "get_define" => Ok(self.pure(Value::str(""))),

            "find_library" => {
                let libname = self.one_string(args.at(0).ok_or_eyre("expected a library name")?)?;
                let required = self.flag(args, "required", self.pc)?;
                let key = format!("lib:{libname}");
                let target = self.external(&key, &libname, External::SystemLibrary {
                    name: libname.to_string(),
                });
                let found = self.dependency_found(&key, &libname, required);
                let value = self.dep_obj(Dep {
                    name: libname.to_string(),
                    found,
                    target,
                    type_name: "library",
                    version: None,
                    variables: Vec::new(),
                });
                Ok(self.pure(value))
            }

            // Whether a warning flag is accepted is a toolchain detail the
            // generated build's own toolchain already decides, so the flags are
            // passed through rather than turned into dozens of knobs.
            "get_supported_arguments" | "get_supported_link_arguments" => {
                let mut items = Vec::new();
                for arg in &args.pos {
                    for v in self.strings(arg)?.into_variants() {
                        items.push(Variant::new(v.cond, Value::Str(v.value)));
                    }
                }
                Ok(self.pure(Value::list(items)))
            }
            "first_supported_argument" => {
                let mut items = Vec::new();
                for arg in &args.pos {
                    for v in self.strings(arg)?.into_variants() {
                        items.push(Variant::new(v.cond, Value::Str(v.value)));
                        break;
                    }
                    break;
                }
                Ok(self.pure(Value::list(items)))
            }
            "has_argument" | "has_link_argument" | "has_multi_arguments"
            | "has_multi_link_arguments" => Ok(self.bool_value(self.pc)),

            other => bail!("a compiler has no method `{other}`"),
        }
    }

    fn compiler_id(&mut self, lang: Lang) -> eyre::Result<Variational<Value>> {
        let choices = self.oracle.compilers();
        if choices.is_empty() {
            bail!("no compilers were configured");
        }
        if choices.len() == 1 {
            return Ok(self.pure(Value::from(choices[0].clone())));
        }

        let id = self.logic.declare(Var {
            key: format!("compiler:{}", lang.as_str()),
            description: Some(format!("the {} compiler in use", lang.as_str())),
            kind: VarKind::Machine,
            choices: choices.clone(),
            default: 0,
        });

        let mut out = Variational::empty();
        for (i, choice) in choices.iter().enumerate() {
            let lit = self.logic.lit(id, i as u32);
            let cond = self.logic.and(self.pc, lit);
            if cond.is_false() {
                continue;
            }
            out.push(Variant::new(cond, Value::from(choice.clone())));
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }

    // -- configuration data -----------------------------------------------

    fn config_method(
        &mut self,
        data: &Rc<std::cell::RefCell<crate::obj::ConfigData>>,
        name: &str,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        match name {
            "set" | "set10" | "set_quoted" => {
                let key = self.one_string(args.at(0).ok_or_eyre("expected an entry name")?)?;
                let value = args.at(1).ok_or_eyre("expected a value")?.clone();

                let mut entries = Variational::empty();
                for variant in value.variants() {
                    let cond = self.logic.and(self.pc, variant.cond);
                    if cond.is_false() {
                        continue;
                    }
                    // A value written into a config header has to be concrete,
                    // so a deferred concatenation is realised here: one entry
                    // per string it can be, each under the condition it is that
                    // string.
                    if let (Value::StrCat(pieces), "set" | "set_quoted") = (&variant.value, name) {
                        let pieces = pieces.clone();
                        for (sub, text) in self.str_cat_realizations(&pieces, cond) {
                            let entry = if name == "set_quoted" {
                                Entry::Quoted(text)
                            } else {
                                Entry::Raw(text)
                            };
                            entries.push(Variant::new(sub, entry));
                        }
                        continue;
                    }
                    let entry = match (name, &variant.value) {
                        ("set_quoted", Value::Str(s)) => Entry::Quoted(s.clone()),
                        ("set10", Value::Bool(b)) => Entry::Ten(*b),
                        ("set10", Value::Int(i)) => Entry::Ten(*i != 0),
                        (_, Value::Str(s)) => Entry::Raw(s.clone()),
                        (_, Value::Int(i)) => Entry::Int(*i),
                        (_, Value::Bool(b)) => Entry::Flag(*b),
                        (_, other) => {
                            bail!("cannot put a {} in a configuration", other.type_name())
                        }
                    };
                    entries.push(Variant::new(cond, entry));
                }

                // The entry keeps whatever it held in the configurations this
                // call does not cover, exactly like a variable assignment.
                let pc = self.pc;
                let elsewhere = self.logic.not(pc);
                let previous = data.borrow().get(&key).cloned().unwrap_or_default();
                entries.extend(previous.restrict(&mut self.logic, elsewhere));
                entries.normalize(&mut self.logic);

                let key: Rc<str> = key.clone();
                *data.borrow_mut().slot(&key) = entries;
                Ok(self.pure(Value::Unset))
            }
            "has" => {
                let key = self.one_string(args.at(0).ok_or_eyre("expected an entry name")?)?;
                let present = data.borrow().get(&key).cloned();
                let cond = match present {
                    Some(v) => v.domain(&mut self.logic),
                    None => Pc::FALSE,
                };
                Ok(self.bool_value(cond))
            }
            "get" | "get_unquoted" => {
                let key = self.one_string(args.at(0).ok_or_eyre("expected an entry name")?)?;
                let entries = data.borrow().get(&key).cloned();
                let Some(entries) = entries else {
                    return args
                        .at(1)
                        .cloned()
                        .ok_or_else(|| eyre::eyre!("no configuration entry `{key}`"));
                };
                let mut out = Variational::empty();
                for entry in entries.variants() {
                    let value = match &entry.value {
                        Entry::Quoted(s) | Entry::Raw(s) => Value::Str(s.clone()),
                        Entry::Int(i) => Value::Int(*i),
                        Entry::Flag(b) => Value::Bool(*b),
                        Entry::Ten(b) => Value::Int(i64::from(*b)),
                    };
                    out.push(Variant::new(entry.cond, value));
                }
                out.normalize(&mut self.logic);
                Ok(out)
            }
            "keys" => {
                let pc = self.pc;
                let keys = data
                    .borrow()
                    .entries
                    .iter()
                    .map(|(k, _)| Variant::new(pc, Value::Str(k.clone())))
                    .collect();
                Ok(self.pure(Value::list(keys)))
            }
            "merge_from" => {
                let other = args.at(0).ok_or_eyre("expected configuration data")?.clone();
                for variant in other.variants() {
                    let Value::Obj(Obj::ConfigData(src)) = &variant.value else {
                        bail!("merge_from() expects configuration_data()");
                    };
                    let cond = self.logic.and(self.pc, variant.cond);
                    let src = src.borrow().clone();
                    for (key, entries) in &src.entries {
                        let restricted = entries.restrict(&mut self.logic, cond);
                        let slot = &mut *data.borrow_mut();
                        slot.slot(key).extend(restricted);
                    }
                }
                Ok(self.pure(Value::Unset))
            }
            other => bail!("configuration data has no method `{other}`"),
        }
    }

    // -- primitives -------------------------------------------------------

    fn str_method(
        &mut self,
        s: &Rc<str>,
        name: &str,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        match name {
            "format" => self.format_positional(s, &args.pos),
            "split" => {
                let pat = match args.at(0) {
                    Some(v) => self.one_string(v)?.to_string(),
                    None => " ".to_owned(),
                };
                let pc = self.pc;
                let items = s
                    .split(pat.as_str())
                    .map(|part| Variant::new(pc, Value::str(part)))
                    .collect();
                Ok(self.pure(Value::list(items)))
            }
            "splitlines" => {
                let pc = self.pc;
                let items = s
                    .lines()
                    .map(|part| Variant::new(pc, Value::str(part)))
                    .collect();
                Ok(self.pure(Value::list(items)))
            }
            "join" => {
                let mut parts = Vec::new();
                for arg in &args.pos {
                    for v in self.strings(arg)?.into_variants() {
                        parts.push(v.value.to_string());
                    }
                }
                Ok(self.pure(Value::from(parts.join(s))))
            }
            "strip" => Ok(self.pure(Value::str(s.trim()))),
            "to_upper" => Ok(self.pure(Value::from(s.to_uppercase()))),
            "to_lower" => Ok(self.pure(Value::from(s.to_lowercase()))),
            "to_int" => Ok(self.pure(Value::Int(s.trim().parse()?))),
            "to_string" => Ok(self.pure(Value::Str(s.clone()))),
            "underscorify" => Ok(self.pure(Value::from(
                s.chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect::<String>(),
            ))),
            "startswith" | "endswith" | "contains" => {
                let pat = self.one_string(args.at(0).ok_or_eyre("expected a string")?)?;
                let holds = match name {
                    "startswith" => s.starts_with(&*pat),
                    "endswith" => s.ends_with(&*pat),
                    _ => s.contains(&*pat),
                };
                Ok(self.bool_value(if holds { self.pc } else { Pc::FALSE }))
            }
            "replace" => {
                let from = self.one_string(args.at(0).ok_or_eyre("expected a string")?)?;
                let to = self.one_string(args.at(1).ok_or_eyre("expected a string")?)?;
                Ok(self.pure(Value::from(s.replace(&*from, &to))))
            }
            "substring" => {
                let start = match args.at(0) {
                    Some(v) => self.one_int(v)?,
                    None => 0,
                };
                let end = match args.at(1) {
                    Some(v) => self.one_int(v)?,
                    None => s.len() as i64,
                };
                let clamp = |i: i64| -> usize {
                    let n = s.len() as i64;
                    (if i < 0 { n + i } else { i }).clamp(0, n) as usize
                };
                let (a, b) = (clamp(start), clamp(end));
                Ok(self.pure(Value::str(&s[a.min(b)..b.max(a)])))
            }
            "version_compare" => {
                let spec = self.one_string(args.at(0).ok_or_eyre("expected a version")?)?;
                let holds = version_compare(s, &spec)?;
                Ok(self.bool_value(if holds { self.pc } else { Pc::FALSE }))
            }
            other => bail!("a string has no method `{other}`"),
        }
    }

    fn int_method(
        &mut self,
        i: i64,
        name: &str,
        _args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        match name {
            "to_string" => Ok(self.pure(Value::from(i.to_string()))),
            "is_even" => Ok(self.bool_value(if i % 2 == 0 { self.pc } else { Pc::FALSE })),
            "is_odd" => Ok(self.bool_value(if i % 2 != 0 { self.pc } else { Pc::FALSE })),
            other => bail!("an int has no method `{other}`"),
        }
    }

    fn bool_method(
        &mut self,
        b: bool,
        name: &str,
        _args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        match name {
            "to_string" => Ok(self.pure(Value::str(if b { "true" } else { "false" }))),
            "to_int" => Ok(self.pure(Value::Int(i64::from(b)))),
            // `cc.run()` really returns a result object with `.returncode()`
            // and friends; a run is modelled as a single probe of whether it
            // behaved as expected, same as `has_function` and the rest, so
            // `.returncode()` on that answer reads the way `== 0` checks on
            // it expect: 0 when it did, 1 when it did not.
            "returncode" => Ok(self.pure(Value::Int(if b { 0 } else { 1 }))),
            // Likewise `.compiled()`: the single probe already stands for "the
            // check behaved as expected", so compilation succeeding is that
            // same answer.
            "compiled" => Ok(self.pure(Value::Bool(b))),
            // The output of a run cannot be known without running it; a project
            // that inspects it is asking for something the importer cannot
            // provide, so give the empty string and let any equality check
            // against it decide.
            "stdout" | "stderr" => Ok(self.pure(Value::str(""))),
            other => bail!("a bool has no method `{other}`"),
        }
    }

    fn list_method(
        &mut self,
        list: &Value,
        name: &str,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        let items = list.as_list().expect("checked by the caller").to_vec();
        match name {
            "length" => {
                // A list whose elements are conditional has no single length,
                // so it is only answerable when every element is unconditional.
                let mut n = 0;
                for item in &items {
                    let unconditional = self.logic.entails(self.pc, item.cond);
                    if !unconditional {
                        bail!(
                            "`.length()` on a list whose contents depend on the configuration \
                             has no single answer"
                        );
                    }
                    n += 1;
                }
                Ok(self.pure(Value::Int(n)))
            }
            "contains" => {
                let needle = args.at(0).ok_or_eyre("expected a value")?.clone();
                let mut cond = Pc::FALSE;
                for item in &items {
                    for want in needle.variants() {
                        if item.value != want.value {
                            continue;
                        }
                        let both = self.logic.and(item.cond, want.cond);
                        cond = self.logic.or(cond, both);
                    }
                }
                Ok(self.bool_value(cond))
            }
            "get" => {
                let index = args.at(0).ok_or_eyre("expected an index")?.clone();
                let fallback = args.at(1).cloned();
                self.index_list(&items, &index, fallback)
            }
            other => bail!("a list has no method `{other}`"),
        }
    }

    fn dict_method(
        &mut self,
        dict: &Value,
        name: &str,
        args: &CallArgs,
    ) -> eyre::Result<Variational<Value>> {
        let Value::Dict(entries) = dict else {
            unreachable!("checked by the caller")
        };
        let entries = entries.to_vec();
        match name {
            "keys" => {
                let items = entries
                    .iter()
                    .map(|e| Variant::new(e.cond, Value::Str(e.value.0.clone())))
                    .collect();
                Ok(self.pure(Value::list(items)))
            }
            "has_key" => {
                let key = self.one_string(args.at(0).ok_or_eyre("expected a key")?)?;
                let mut cond = Pc::FALSE;
                for entry in &entries {
                    if entry.value.0 == key {
                        cond = self.logic.or(cond, entry.cond);
                    }
                }
                Ok(self.bool_value(cond))
            }
            "get" => {
                let key = self.one_string(args.at(0).ok_or_eyre("expected a key")?)?;
                let mut out = Variational::empty();
                for entry in &entries {
                    if entry.value.0 == key {
                        out.push(Variant::new(entry.cond, entry.value.1.clone()));
                    }
                }
                if out.is_empty() {
                    return args
                        .at(1)
                        .cloned()
                        .ok_or_else(|| eyre::eyre!("no dict entry `{key}`"));
                }
                out.normalize(&mut self.logic);
                Ok(out)
            }
            other => bail!("a dict has no method `{other}`"),
        }
    }

    // -- indexing ---------------------------------------------------------

    pub(crate) fn index(
        &mut self,
        obj: &Variational<Value>,
        index: &Variational<Value>,
    ) -> eyre::Result<Variational<Value>> {
        let mut out = Variational::empty();
        for variant in obj.variants().to_vec() {
            let cond = self.logic.and(self.pc, variant.cond);
            if cond.is_false() {
                continue;
            }
            let result = match &variant.value {
                Value::List(items) => {
                    let items = items.to_vec();
                    self.with_pc(cond, |this| this.index_list(&items, index, None))?
                }
                Value::Dict(entries) => {
                    let entries = entries.to_vec();
                    let key = self.one_string(index)?;
                    let mut found = Variational::empty();
                    for entry in &entries {
                        if entry.value.0 == key {
                            found.push(Variant::new(entry.cond, entry.value.1.clone()));
                        }
                    }
                    if found.is_empty() {
                        bail!("no dict entry `{key}`");
                    }
                    found
                }
                Value::Str(s) => {
                    let i = self.one_int(index)?;
                    let ch = s
                        .chars()
                        .nth(usize::try_from(i)?)
                        .ok_or_eyre("string index out of range")?;
                    self.pure(Value::from(ch.to_string()))
                }
                // A multi-output custom target indexes to one of its outputs.
                Value::Obj(Obj::Target(id)) => {
                    let i = self.one_int(index)?;
                    self.pure(Value::Obj(Obj::Output(*id, usize::try_from(i)?)))
                }
                other => bail!("cannot index a {}", other.type_name()),
            };
            out.extend(result.restrict(&mut self.logic, cond));
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }

    /// Index a list whose elements may each be conditional.
    ///
    /// Positions only line up when the elements before the one being asked for
    /// are unconditional; otherwise the index means different things in
    /// different configurations and the sources have to be restructured.
    fn index_list(
        &mut self,
        items: &[Variant<Value>],
        index: &Variational<Value>,
        fallback: Option<Variational<Value>>,
    ) -> eyre::Result<Variational<Value>> {
        let mut out = Variational::empty();
        for want in index.variants() {
            let i = want
                .value
                .as_int()
                .ok_or_eyre("expected an integer index")?;
            let n = items.len() as i64;
            let i = if i < 0 { n + i } else { i };

            let Some(item) = usize::try_from(i).ok().and_then(|i| items.get(i)) else {
                match &fallback {
                    Some(v) => {
                        out.extend(v.clone().into_variants());
                        continue;
                    }
                    None => bail!("index {i} is out of range for a list of {n}"),
                }
            };

            for earlier in &items[..usize::try_from(i)?] {
                if !self.logic.entails(self.pc, earlier.cond) {
                    bail!(
                        "indexing past an element that only exists in some configurations \
                         would select different values in different builds"
                    );
                }
            }

            let cond = self.logic.and(want.cond, item.cond);
            out.push(Variant::new(cond, item.value.clone()));
        }
        out.normalize(&mut self.logic);
        Ok(out)
    }
}

/// Meson's `version_compare`: an operator followed by a dotted version.
fn version_compare(have: &str, spec: &str) -> eyre::Result<bool> {
    let (op, want) = spec
        .find(|c: char| c.is_ascii_digit())
        .map(|i| spec.split_at(i))
        .ok_or_eyre("a version constraint needs a version")?;

    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| p.trim_matches(|c: char| !c.is_ascii_digit()))
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parse(have), parse(want.trim()));
    let ord = a.cmp(&b);

    use std::cmp::Ordering::*;
    Ok(match op.trim() {
        ">=" | "" => ord != Less,
        ">" => ord == Greater,
        "<=" => ord != Greater,
        "<" => ord == Less,
        "==" | "=" => ord == Equal,
        "!=" => ord != Equal,
        other => bail!("unknown version operator `{other}`"),
    })
}
