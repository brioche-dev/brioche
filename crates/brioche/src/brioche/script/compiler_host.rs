use std::{cell::RefCell, rc::Rc, sync::Arc};

use anyhow::Context as _;
use deno_core::OpState;

use crate::brioche::Brioche;

use super::{
    specifier::{BriocheImportSpecifier, BriocheModuleSpecifier},
    vfs::Vfs,
};

#[derive(Clone)]
pub struct BriocheCompilerHost {
    pub brioche: Brioche,
    pub vfs: Vfs,
}

impl BriocheCompilerHost {
    pub fn new(brioche: Brioche) -> Self {
        Self {
            brioche: brioche.clone(),
            vfs: Vfs::new(brioche),
        }
    }
}

deno_core::extension!(brioche_compiler_host,
    ops = [
        op_brioche_file_read,
        op_brioche_file_exists,
        op_brioche_file_version,
        op_brioche_resolve_module,
    ],
    options = {
        compiler_host: BriocheCompilerHost,
    },
    state = |state, options| {
        state.put(options.compiler_host);
    },
);

fn brioche_compiler_host_state(state: Rc<RefCell<OpState>>) -> anyhow::Result<BriocheCompilerHost> {
    let state = state.try_borrow()?;
    let compiler_host = state
        .try_borrow::<BriocheCompilerHost>()
        .context("failed to get compiler host instance")?
        .clone();

    Ok(compiler_host)
}

#[deno_core::op]
pub fn op_brioche_file_read(
    state: Rc<RefCell<OpState>>,
    path: &str,
) -> anyhow::Result<Option<Arc<String>>> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let contents = compiler_host
        .vfs
        .read_loaded_document(specifier, |doc| doc.contents.clone())?;
    Ok(contents)
}

#[deno_core::op]
pub fn op_brioche_file_exists(state: Rc<RefCell<OpState>>, path: &str) -> anyhow::Result<bool> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let result = compiler_host.vfs.read_loaded_document(specifier, |_| ())?;
    Ok(result.is_some())
}

#[deno_core::op]
pub fn op_brioche_file_version(
    state: Rc<RefCell<OpState>>,
    path: &str,
) -> anyhow::Result<Option<u64>> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let version = compiler_host
        .vfs
        .read_loaded_document(specifier, |doc| doc.version)?;
    Ok(version)
}

#[deno_core::op]
pub fn op_brioche_resolve_module(
    state: Rc<RefCell<OpState>>,
    specifier: &str,
    referrer: &str,
) -> Option<String> {
    let compiler_host = brioche_compiler_host_state(state).ok()?;

    let referrer: BriocheModuleSpecifier = referrer.parse().ok()?;
    let specifier: BriocheImportSpecifier = specifier.parse().ok()?;
    let resolved =
        super::specifier::resolve_sync(&compiler_host.brioche, &specifier, &referrer).ok()?;

    Some(resolved.to_string())
}
