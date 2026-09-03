use {
    eyre::{
        Context,
        OptionExt, //
    },
    pyo3::{
        ffi::c_str,
        prelude::*,
        sync::PyOnceLock,
        types::PyModule, //
    },
    std::{
        ffi::CStr,
        path::Path,
        sync::Mutex, //
    },
};

static PARSE_BUILD_PY: &CStr = c_str!(include_str!("py/parse_build.py"));
static PARSE_OPTIONS_PY: &CStr = c_str!(include_str!("py/parse_options.py"));

/// The embedded parser scripts are compiled once and reused. `PyModule::from_code`
/// recompiles the source on every call otherwise, which for a project with many
/// `meson.build` files is the bulk of parsing time.
static PARSE_BUILD_MOD: PyOnceLock<Py<PyModule>> = PyOnceLock::new();
static PARSE_OPTIONS_MOD: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

/// One meson parse at a time, process-wide.
///
/// Every parse runs embedded CPython, which the GIL already serialises for
/// bytecode — but *not* for the import machinery. Two threads first-importing
/// `mesonbuild` (and its stdlib dependencies) at once race, and leave a module
/// half-initialised ("partially initialized module 'typing' ... circular
/// import"). Holding this lock across the whole `Python::attach` closure keeps
/// that from happening; it costs almost nothing, since the GIL means the two
/// could not have run in parallel anyway. A caller's non-Python work (the
/// executor's own interpretation) still overlaps freely.
static PARSE_LOCK: Mutex<()> = Mutex::new(());

/// Force the embedded interpreter and both parser modules to initialise now, on
/// the calling thread, so worker threads later only ever hit the cached path.
/// Cheap and idempotent.
pub fn warmup() -> eyre::Result<()> {
    let _guard = PARSE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Python::attach(|py| -> PyResult<()> {
        module(
            py,
            &PARSE_BUILD_MOD,
            PARSE_BUILD_PY,
            c"parse_build.py",
            c"parse_build",
        )?;
        module(
            py,
            &PARSE_OPTIONS_MOD,
            PARSE_OPTIONS_PY,
            c"parse_options.py",
            c"parse_options",
        )?;
        Ok(())
    })
    .wrap_err("Failed to warm up the meson parser")
}

fn module<'py>(
    py: Python<'py>,
    cell: &PyOnceLock<Py<PyModule>>,
    code: &CStr,
    file_name: &CStr,
    module_name: &CStr,
) -> PyResult<Bound<'py, PyModule>> {
    cell.get_or_try_init(py, || {
        PyModule::from_code(py, code, file_name, module_name).map(Bound::unbind)
    })
    .map(|m| m.bind(py).clone())
}

fn parse_with(
    cell: &PyOnceLock<Py<PyModule>>,
    code: &CStr,
    file_name: &CStr,
    module_name: &CStr,
    path: &Path,
) -> eyre::Result<String> {
    let _guard = PARSE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Python::attach(|py| {
        module(py, cell, code, file_name, module_name)?
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

pub(crate) fn parse_build(path: &Path) -> eyre::Result<String> {
    parse_with(
        &PARSE_BUILD_MOD,
        PARSE_BUILD_PY,
        c"parse_build.py",
        c"parse_build",
        path,
    )
}

pub(crate) fn parse_options(path: &Path) -> eyre::Result<String> {
    parse_with(
        &PARSE_OPTIONS_MOD,
        PARSE_OPTIONS_PY,
        c"parse_options.py",
        c"parse_options",
        path,
    )
}
