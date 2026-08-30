use {
    eyre::{
        Context,
        OptionExt, //
    },
    pyo3::{
        ffi::c_str,
        prelude::*, //
    },
    std::{
        ffi::CStr,
        path::Path, //
    },
};

static PARSE_BUILD_PY: &CStr = c_str!(include_str!("py/parse_build.py"));
static PARSE_OPTIONS_PY: &CStr = c_str!(include_str!("py/parse_options.py"));

pub(crate) fn parse_build(path: &Path) -> eyre::Result<String> {
    Python::attach(|py| {
        let module = PyModule::from_code(py, PARSE_BUILD_PY, c"parse_build.py", c"parse_build")?;
        module
            .getattr("parse")?
            .call1((path
                .to_str()
                .ok_or_eyre("Failed to convert Path into a String")?,))?
            .extract::<String>()
            .wrap_err_with(|| path.display().to_string())
            .wrap_err("Failed to parse meson file")
    })
    .wrap_err("Failed to attach Python interpreter")
}

pub(crate) fn parse_options(path: &Path) -> eyre::Result<String> {
    Python::attach(|py| {
        let module =
            PyModule::from_code(py, PARSE_OPTIONS_PY, c"parse_options.py", c"parse_options")?;
        module
            .getattr("parse")?
            .call1((path
                .to_str()
                .ok_or_eyre("Failed to convert Path into a String")?,))?
            .extract::<String>()
            .wrap_err_with(|| path.display().to_string())
            .wrap_err("Failed to parse meson file")
    })
    .wrap_err("Failed to attach Python interpreter")
}
