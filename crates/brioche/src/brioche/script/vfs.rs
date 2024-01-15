use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use tokio::io::AsyncReadExt as _;

use crate::brioche::{project::analyze::find_imports, Brioche};

use super::specifier::BriocheModuleSpecifier;

#[derive(Clone)]
pub struct Vfs {
    pub brioche: Brioche,
    pub documents: Arc<RwLock<HashMap<BriocheModuleSpecifier, BriocheDocument>>>,
}

impl Vfs {
    pub fn new(brioche: Brioche) -> Self {
        let documents: HashMap<_, _> = super::specifier::runtime_specifiers_with_contents()
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
            brioche,
            documents: Arc::new(RwLock::new(documents)),
        }
    }

    pub async fn load_document(&self, specifier: &BriocheModuleSpecifier) -> anyhow::Result<()> {
        let mut already_visited = HashSet::new();
        let mut specifiers_to_load = vec![specifier.clone()];

        while let Some(specifier) = specifiers_to_load.pop() {
            if !already_visited.insert(specifier.clone()) {
                continue;
            }

            let contents = {
                let documents = self
                    .documents
                    .read()
                    .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
                documents.get(&specifier).map(|doc| doc.contents.clone())
            };

            let contents = match contents {
                Some(contents) => contents,
                None => {
                    let Some(mut reader) =
                        super::specifier::read_specifier_contents(&specifier).await?
                    else {
                        tracing::warn!("failed to load document {specifier}");
                        return Ok(());
                    };
                    let mut contents = String::new();
                    reader.read_to_string(&mut contents).await?;

                    let mut documents = self
                        .documents
                        .write()
                        .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;

                    match documents.entry(specifier.clone()) {
                        std::collections::hash_map::Entry::Occupied(entry) => {
                            entry.get().contents.clone()
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            tracing::info!("loaded new document into vfs: {specifier}");
                            let contents = Arc::new(contents);
                            entry.insert(BriocheDocument {
                                contents: contents.clone(),
                                version: 0,
                            });
                            contents
                        }
                    }
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
                let resolved =
                    super::specifier::resolve(&self.brioche, &import_specifier, &specifier).await;
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
            let mut documents = self
                .documents
                .write()
                .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
            documents
                .entry(specifier.clone())
                .and_modify(|doc| {
                    if *doc.contents != contents {
                        doc.version += 1;

                        let doc_contents = Arc::make_mut(&mut doc.contents);
                        *doc_contents = contents.to_owned();
                    }
                })
                .or_insert_with(|| {
                    tracing::info!("inserted new document into vfs: {specifier}");
                    BriocheDocument {
                        contents: Arc::new(contents.to_owned()),
                        version: 0,
                    }
                });
        }

        self.load_document(&specifier).await?;

        Ok(())
    }

    pub fn read_loaded_document<R>(
        &self,
        specifier: BriocheModuleSpecifier,
        f: impl FnOnce(&BriocheDocument) -> R,
    ) -> anyhow::Result<Option<R>> {
        let documents = self
            .documents
            .read()
            .map_err(|_| anyhow::anyhow!("failed to acquire lock on documents"))?;
        let Some(document) = documents.get(&specifier) else {
            return Ok(None);
        };

        let result = f(document);
        Ok(Some(result))
    }
}

#[derive(Debug, Clone)]
pub struct BriocheDocument {
    pub contents: Arc<String>,
    version: u64,
}

impl BriocheDocument {
    pub fn version(&self) -> u64 {
        self.version
    }
}
