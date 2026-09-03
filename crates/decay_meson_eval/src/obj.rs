use {
    eyre::bail,
    std::str::FromStr, //
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obj {
    Meson,
    Machine(Machine),
    Compiler(Lang),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lang {
    C,
}

impl FromStr for Lang {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "c" => Lang::C,
            _ => bail!("unknown language {s}"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Machine {
    Host,
}
