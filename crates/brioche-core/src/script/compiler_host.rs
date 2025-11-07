#![expect(clippy::needless_pass_by_value)]

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use anyhow::Context as _;
use deno_core::OpState;

use crate::project::analyze::find_imports;

use super::{
    bridge::RuntimeBridge,
    specifier::{self, BriocheImportSpecifier, BriocheModuleSpecifier},
};

#[derive(Clone)]
pub struct BriocheCompilerHost {
    pub bridge: RuntimeBridge,
    pub documents: Arc<tokio::sync::RwLock<HashMap<BriocheModuleSpecifier, BriocheDocument>>>,
}

impl BriocheCompilerHost {
    pub fn new(bridge: RuntimeBridge) -> Self {
        let documents: HashMap<_, _> = specifier::runtime_specifiers_with_contents()
            .map(|(specifier, contents)| {
                let contents = std::str::from_utf8(&contents)
                    .map_err(|_| anyhow::anyhow!("invalid UTF-8 in runtime file: {specifier}"))
                    .unwrap();
                let document = BriocheDocument {
                    contents: Arc::new(contents.to_owned()),
                    version: 0,
                };
                (specifier, document)
            })
            .collect();

        Self {
            bridge,
            documents: Arc::new(tokio::sync::RwLock::new(documents)),
        }
    }

    pub async fn is_document_loaded(
        &self,
        specifier: &BriocheModuleSpecifier,
    ) -> anyhow::Result<bool> {
        let documents = self.documents.read().await;

        Ok(documents.contains_key(specifier))
    }

    pub async fn load_documents(
        &self,
        specifiers: Vec<BriocheModuleSpecifier>,
    ) -> anyhow::Result<()> {
        let mut already_visited = HashSet::new();
        let mut specifiers_to_load = specifiers.clone();
        let mut documents = self.documents.write().await;

        while let Some(specifier) = specifiers_to_load.pop() {
            if !already_visited.insert(specifier.clone()) {
                continue;
            }

            let document_entry = documents.entry(specifier.clone());

            match &specifier {
                BriocheModuleSpecifier::File { path } => {
                    self.bridge
                        .load_project_from_module_path(path.clone())
                        .await?;
                }
                BriocheModuleSpecifier::Runtime { .. } => {}
            }

            let contents = match document_entry {
                std::collections::hash_map::Entry::Occupied(entry) => entry.get().contents.clone(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let contents = self
                        .bridge
                        .load_specifier_contents(specifier.clone())
                        .await?;
                    let contents = std::str::from_utf8(&contents).with_context(|| {
                        format!("failed to parse module '{specifier}' contents as UTF-8 string")
                    })?;
                    let contents = Arc::new(contents.to_string());
                    entry.insert(BriocheDocument {
                        contents: contents.clone(),
                        version: 0,
                    });

                    tracing::debug!("loaded new document into compiler host: {specifier}");

                    contents
                }
            };

            let import_specifiers = {
                let parsed = biome_js_parser::parse(
                    &contents,
                    biome_js_syntax::JsFileSource::ts()
                        .with_module_kind(biome_js_syntax::ModuleKind::Module),
                    biome_js_parser::JsParserOptions::default(),
                )
                .cast::<biome_js_syntax::JsModule>()
                .expect("failed to cast module");

                let Some(parsed_module) = parsed.try_tree() else {
                    tracing::warn!("failed to parse module {specifier}");
                    return Ok(());
                };

                find_imports(&parsed_module, |_| "<unknown>").collect::<Vec<_>>()
            };

            for import_specifier in import_specifiers {
                let import_specifier = match import_specifier {
                    Ok(import_specifier) => import_specifier,
                    Err(error) => {
                        tracing::warn!("error parsing import specifier: {error:#}");
                        continue;
                    }
                };

                let resolved = tokio::task::spawn_blocking({
                    let bridge = self.bridge.clone();
                    let specifier = specifier.clone();
                    move || bridge.resolve_specifier(import_specifier, specifier)
                })
                .await?;

                let resolved = match resolved {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        tracing::warn!("error resolving import specifier: {error:#}");
                        continue;
                    }
                };

                specifiers_to_load.push(resolved.clone());
            }
        }

        Ok(())
    }

    pub async fn update_document(&self, uri: &url::Url, contents: &str) -> anyhow::Result<()> {
        let specifier: BriocheModuleSpecifier = uri.try_into()?;

        {
            let mut documents = self.documents.write().await;
            documents
                .entry(specifier.clone())
                .and_modify(|doc| {
                    if *doc.contents != contents {
                        doc.version += 1;

                        let doc_contents = Arc::make_mut(&mut doc.contents);
                        contents.clone_into(doc_contents);
                    }
                })
                .or_insert_with(|| {
                    tracing::debug!("inserted new document into compiler host: {specifier}");
                    BriocheDocument {
                        contents: Arc::new(contents.to_owned()),
                        version: 0,
                    }
                });
        }

        self.bridge
            .update_vfs_contents(specifier.clone(), Arc::new(contents.as_bytes().to_vec()))
            .await?;

        self.load_documents(vec![specifier]).await?;

        Ok(())
    }

    pub async fn read_loaded_document<R>(
        &self,
        specifier: &BriocheModuleSpecifier,
        f: impl FnOnce(&BriocheDocument) -> R,
    ) -> anyhow::Result<Option<R>> {
        let documents = self.documents.read().await;
        let Some(document) = documents.get(specifier) else {
            return Ok(None);
        };

        let result = f(document);
        Ok(Some(result))
    }

    fn read_loaded_document_sync<R: Send + 'static>(
        &self,
        specifier: &BriocheModuleSpecifier,
        f: impl FnOnce(&BriocheDocument) -> R + Send + 'static,
    ) -> anyhow::Result<Option<R>> {
        if let Ok(documents) = self.documents.try_read() {
            let Some(document) = documents.get(specifier) else {
                return Ok(None);
            };
            let result = f(document);
            Ok(Some(result))
        } else {
            let result = std::thread::spawn({
                let rt = tokio::runtime::Handle::current();
                let host = self.clone();
                let specifier = specifier.clone();
                move || rt.block_on(host.read_loaded_document(&specifier, f))
            })
            .join()
            .map_err(|_| anyhow::anyhow!("failed to wait for loaded document"))??;
            Ok(result)
        }
    }

    pub async fn reload_module_project(&self, uri: &url::Url) -> anyhow::Result<()> {
        let specifier: BriocheModuleSpecifier = uri.try_into()?;

        let did_update = self
            .bridge
            .reload_project_from_specifier(specifier.clone())
            .await?;

        if did_update {
            let mut documents = self.documents.write().await;
            documents.entry(specifier.clone()).and_modify(|doc| {
                doc.version += 1;
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BriocheDocument {
    pub contents: Arc<String>,
    pub version: u64,
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

fn brioche_compiler_host_state(
    state: &Rc<RefCell<OpState>>,
) -> anyhow::Result<BriocheCompilerHost> {
    let state = state.try_borrow()?;
    let compiler_host = state
        .try_borrow::<BriocheCompilerHost>()
        .context("failed to get compiler host instance")?
        .clone();

    Ok(compiler_host)
}

#[deno_core::op2]
#[serde]
pub fn op_brioche_file_read(
    state: Rc<RefCell<OpState>>,
    #[string] path: &str,
) -> Result<Option<Arc<String>>, super::AnyError> {
    let compiler_host = brioche_compiler_host_state(&state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let contents =
        compiler_host.read_loaded_document_sync(&specifier, |doc| doc.contents.clone())?;
    Ok(contents)
}

#[deno_core::op2(fast)]
pub fn op_brioche_file_exists(
    state: Rc<RefCell<OpState>>,
    #[string] path: &str,
) -> Result<bool, super::AnyError> {
    let compiler_host = brioche_compiler_host_state(&state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let result = compiler_host.read_loaded_document_sync(&specifier, |_| ())?;
    Ok(result.is_some())
}

#[deno_core::op2]
#[bigint]
pub fn op_brioche_file_version(
    state: Rc<RefCell<OpState>>,
    #[string] path: &str,
) -> Result<Option<u64>, super::AnyError> {
    let compiler_host = brioche_compiler_host_state(&state)?;

    let specifier: BriocheModuleSpecifier = path.parse()?;

    let version = compiler_host.read_loaded_document_sync(&specifier, |doc| doc.version)?;
    Ok(version)
}

#[deno_core::op2]
#[string]
pub fn op_brioche_resolve_module(
    state: Rc<RefCell<OpState>>,
    #[string] specifier: &str,
    #[string] referrer: &str,
) -> Option<String> {
    let compiler_host = brioche_compiler_host_state(&state).ok()?;

    let referrer: BriocheModuleSpecifier = referrer.parse().ok()?;
    let specifier: BriocheImportSpecifier = specifier.parse().ok()?;

    let resolved = compiler_host
        .bridge
        .resolve_specifier(specifier, referrer)
        .ok()?;

    Some(resolved.to_string())
}
