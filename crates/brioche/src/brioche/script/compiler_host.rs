use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, RwLock},
};

use anyhow::Context as _;
use deno_core::OpState;

use crate::brioche::Brioche;

use super::specifier::{BriocheImportSpecifier, BriocheModuleSpecifier};

#[derive(Clone)]
pub struct BriocheCompilerHost {
    pub brioche: Brioche,
    pub documents: Arc<RwLock<HashMap<BriocheModuleSpecifier, BriocheDocument>>>,
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
            .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
        documents
            .entry(specifier)
            .and_modify(|doc| {
                if doc.contents != contents {
                    doc.version += 1;
                    doc.contents = contents.to_owned();
                }
            })
            .or_insert_with(|| BriocheDocument {
                contents: contents.to_owned(),
                version: 0,
            });

        Ok(())
    }

    pub fn read_document_sync<R>(
        &self,
        specifier: BriocheModuleSpecifier,
        f: impl FnOnce(&BriocheDocument) -> R,
    ) -> anyhow::Result<Option<R>> {
        {
            let documents = self
                .documents
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
            if let Some(document) = documents.get(&specifier) {
                let result = f(document);
                return Ok(Some(result));
            }
        }

        let Some(mut reader) = super::specifier::read_specifier_contents_sync(&specifier)? else {
            return Ok(None);
        };

        let mut contents = String::new();
        reader.read_to_string(&mut contents)?;

        let mut documents = self
            .documents
            .write()
            .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
        let document = documents
            .entry(specifier)
            .or_insert_with(|| BriocheDocument {
                contents: contents.to_owned(),
                version: 0,
            });

        let result = f(document);
        Ok(Some(result))
    }
}

#[derive(Debug, Clone)]
pub struct BriocheDocument {
    contents: String,
    version: u64,
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
) -> anyhow::Result<Option<String>> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let contents = compiler_host.read_document_sync(specifier, |doc| doc.contents.clone())?;
    Ok(contents)
}

#[deno_core::op]
pub fn op_brioche_file_exists(state: Rc<RefCell<OpState>>, path: &str) -> anyhow::Result<bool> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let result = compiler_host.read_document_sync(specifier, |_| ())?;
    Ok(result.is_some())
}

#[deno_core::op]
pub fn op_brioche_file_version(
    state: Rc<RefCell<OpState>>,
    path: &str,
) -> anyhow::Result<Option<u64>> {
    let compiler_host = brioche_compiler_host_state(state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let version = compiler_host.read_document_sync(specifier, |doc| doc.version)?;
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
    let resolved = super::specifier::resolve(&compiler_host.brioche, &specifier, &referrer).ok()?;

    Some(resolved.to_string())
}
