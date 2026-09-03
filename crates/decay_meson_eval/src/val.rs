use {
    crate::obj::Obj,
    decay_meson_logic::Variant,
    std::{
        fmt::{
            self,
            Display, //
        },
        rc::Rc,
    },
};

/// A meson value at one point in the configuration space.
///
/// Lists and dicts hold *conditional* entries rather than being split into one
/// variant per shape. Keeping the condition on the element is what stops a
/// dozen independent `if`s appending to the same list from turning into a
/// thousand list variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A variable that exists but holds nothing meaningful, e.g. the result of
    /// a call made only for its effect.
    Unset,
    Bool(bool),
    Int(i64),
    Str(Rc<str>),
    List(Rc<Vec<Variant<Value>>>),
    Dict(Rc<Vec<Variant<(Rc<str>, Value)>>>),
    Obj(Obj),
}

impl Value {
    pub fn list(items: Vec<Variant<Value>>) -> Self {
        Self::List(Rc::new(items))
    }

    pub fn dict(items: Vec<Variant<(Rc<str>, Value)>>) -> Self {
        Self::Dict(Rc::new(items))
    }

    pub fn str(s: impl AsRef<str>) -> Self {
        Self::Str(Rc::from(s.as_ref()))
    }

    pub fn as_str(&self) -> Option<&Rc<str>> {
        match self {
            Self::Str(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Variant<Value>]> {
        match self {
            Self::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Self::Obj(v) => Some(v),
            _ => None,
        }
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Self::List(_))
    }

    /// The name meson would use for this value's type in an error message.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Str(_) => "str",
            Self::List(_) => "list",
            Self::Dict(_) => "dict",
            Self::Obj(o) => o.type_name(),
        }
    }
}

impl Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unset => f.write_str("<unset>"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Str(v) => f.write_str(v),
            Self::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", item.value)?;
                }
                f.write_str("]")
            }
            Self::Dict(items) => {
                f.write_str("{")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", item.value.0, item.value.1)?;
                }
                f.write_str("}")
            }
            Self::Obj(o) => write!(f, "<{}>", o.type_name()),
        }
    }
}

impl From<&'_ str> for Value {
    fn from(value: &'_ str) -> Self {
        Self::Str(Rc::from(value))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Str(Rc::from(value.as_str()))
    }
}

impl From<&'_ String> for Value {
    fn from(value: &'_ String) -> Self {
        Self::Str(Rc::from(value.as_str()))
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}
