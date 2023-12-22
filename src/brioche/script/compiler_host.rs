use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, RwLock},
};

use anyhow::Context as _;
use deno_core::OpState;

use crate::brioche::Brioche;

use super::specifier::BriocheModuleSpecifier;

#[derive(Clone)]
pub struct BriocheCompilerHost {
    pub brioche: Brioche,
    pub documents: Arc<RwLock<HashMap<BriocheModuleSpecifier, String>>>,
}

impl BriocheCompilerHost {
    pub fn new(brioche: Brioche) -> Self {
        Self {
            brioche,
            documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn update_document(&self, uri: &url::Url, contents: &str) -> anyhow::Result<()> {
        let specifier: BriocheModuleSpecifier = uri.try_into()?;

        let mut documents = self
            .documents
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire lock on docuemnts"))?;
        documents.insert(specifier, contents.to_owned());

        Ok(())
    }
}

deno_core::extension!(brioche_compiler_host,
    ops = [
        op_brioche_file_read,
        op_brioche_file_exists,
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
) -> anyhow::Result<Option<String>> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    {
        let documents = compiler_host
            .documents
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire lock on docuemnts"))?;
        if let Some(contents) = documents.get(&specifier) {
            return Ok(Some(contents.to_owned()));
        }
    }

    let Some(mut reader) = super::specifier::read_specifier_contents_sync(&specifier)? else {
        return Ok(None);
    };

    let mut contents = String::new();
    reader.read_to_string(&mut contents)?;
    Ok(Some(contents))
}

#[deno_core::op]
pub fn op_brioche_file_exists(state: Rc<RefCell<OpState>>, path: &str) -> anyhow::Result<bool> {
    let _compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;
    let reader = super::specifier::read_specifier_contents_sync(&specifier)?;

    Ok(reader.is_some())
}

#[deno_core::op]
pub fn op_brioche_resolve_module(
    state: Rc<RefCell<OpState>>,
    specifier: &str,
    referrer: &str,
) -> Option<String> {
    let compiler_host = brioche_compiler_host_state(state).ok()?;

    let referrer: BriocheModuleSpecifier = referrer.parse().ok()?;
    let resolved = super::specifier::resolve(&compiler_host.brioche, specifier, &referrer).ok()?;

    Some(resolved.to_string())
}
