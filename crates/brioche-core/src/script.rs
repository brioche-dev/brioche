use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{hash_map::Entry, HashMap},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use anyhow::Context as _;
use bridge::RuntimeBridge;
use deno_core::OpState;
use specifier::BriocheModuleSpecifier;

use crate::{bake::BakeScope, project::analyze::StaticQuery};

use super::{
    blob::BlobHash,
    recipe::{Artifact, Recipe, WithMeta},
    script::specifier::BriocheImportSpecifier,
};

mod bridge;
pub mod check;
mod compiler_host;
pub mod evaluate;
pub mod format;
mod js;
pub mod lsp;
pub mod specifier;

#[derive(Clone)]
struct BriocheModuleLoader {
    pub bridge: RuntimeBridge,
    pub sources: Rc<RefCell<HashMap<BriocheModuleSpecifier, ModuleSource>>>,
}

impl BriocheModuleLoader {
    fn new(bridge: RuntimeBridge) -> Self {
        Self {
            bridge,
            sources: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn load_module_source(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
    ) -> Result<deno_core::ModuleSource, anyhow::Error> {
        let brioche_module_specifier: BriocheModuleSpecifier = module_specifier.try_into()?;
        let contents = self
            .bridge
            .read_specifier_contents(brioche_module_specifier.clone())?;

        let code = std::str::from_utf8(&contents)
            .context("failed to parse module contents as UTF-8 string")?;

        let parsed = deno_ast::parse_module(deno_ast::ParseParams {
            specifier: brioche_module_specifier.clone().into(),
            text: code.into(),
            media_type: deno_ast::MediaType::TypeScript,
            capture_tokens: false,
            scope_analysis: false,
            maybe_syntax: None,
        })?;
        let transpiled = parsed.transpile(
            &deno_ast::TranspileOptions {
                imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Preserve,
                ..Default::default()
            },
            &deno_ast::TranspileModuleOptions {
                module_kind: Some(deno_ast::ModuleKind::Esm),
            },
            &deno_ast::EmitOptions {
                source_map: deno_ast::SourceMapOption::Separate,
                ..Default::default()
            },
        )?;

        if let Entry::Vacant(entry) = self
            .sources
            .borrow_mut()
            .entry(brioche_module_specifier.clone())
        {
            let source_map = transpiled
                .clone()
                .into_source()
                .source_map
                .context("source map not generated")?;
            entry.insert(ModuleSource {
                source_contents: contents.clone(),
                source_map: source_map.into_bytes(),
            });
        }

        Ok(deno_core::ModuleSource::new(
            deno_core::ModuleType::JavaScript,
            deno_core::ModuleSourceCode::String(transpiled.into_source().text.into()),
            module_specifier,
            None,
        ))
    }
}

impl deno_core::ModuleLoader for BriocheModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
    ) -> Result<deno_core::ModuleSpecifier, anyhow::Error> {
        if let deno_core::ResolutionKind::MainModule = kind {
            let resolved = specifier.parse()?;
            tracing::debug!(%specifier, %referrer, %resolved, "resolved main module");
            return Ok(resolved);
        }

        let referrer: BriocheModuleSpecifier = referrer.parse()?;
        let specifier: BriocheImportSpecifier = specifier.parse()?;

        let resolved = self
            .bridge
            .resolve_specifier(specifier.clone(), referrer.clone())?;

        tracing::debug!(%specifier, %referrer, %resolved, "resolved module");

        let resolved: deno_core::ModuleSpecifier = resolved.into();
        Ok(resolved)
    }

    fn load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        _maybe_referrer: Option<&deno_core::ModuleSpecifier>,
        _is_dyn_import: bool,
        _requested_module_type: deno_core::RequestedModuleType,
    ) -> deno_core::ModuleLoadResponse {
        deno_core::ModuleLoadResponse::Sync(self.load_module_source(module_specifier))
    }

    fn get_source_map(&self, file_name: &str) -> Option<Cow<[u8]>> {
        let sources = self.sources.borrow();
        let specifier: BriocheModuleSpecifier = file_name.parse().ok()?;
        let code = sources.get(&specifier)?;
        let source_map = code.source_map.clone();
        Some(source_map.into())
    }

    fn get_source_mapped_source_line(&self, file_name: &str, line_number: usize) -> Option<String> {
        let sources = self.sources.borrow();
        let specifier: BriocheModuleSpecifier = file_name.parse().ok()?;
        let source = sources.get(&specifier)?;
        let code = std::str::from_utf8(&source.source_contents)
            .inspect_err(|err| {
                tracing::warn!("failed to parse source contents of {file_name} as UTF-8: {err}");
            })
            .ok()?;
        let line = code.lines().nth(line_number)?;
        Some(line.to_string())
    }
}

struct ModuleSource {
    pub source_contents: Arc<Vec<u8>>,
    pub source_map: Vec<u8>,
}

pub enum ModuleLoaderMessage {
    ReadSpecifierContents {
        specifier: BriocheModuleSpecifier,
        contents_tx: std::sync::mpsc::Sender<anyhow::Result<Arc<Vec<u8>>>>,
    },
    ResolveSpecifier {
        specifier: BriocheImportSpecifier,
        referrer: BriocheModuleSpecifier,
        resolved_tx: std::sync::mpsc::Sender<anyhow::Result<BriocheModuleSpecifier>>,
    },
    ProjectRootForModulePath {
        module_path: PathBuf,
        project_path_tx: tokio::sync::oneshot::Sender<anyhow::Result<PathBuf>>,
    },
}

deno_core::extension!(brioche_rt,
    ops = [
        op_brioche_bake_all,
        op_brioche_create_proxy,
        op_brioche_read_blob,
        op_brioche_get_static,
    ],
    options = {
        bridge: RuntimeBridge,
        bake_scope: BakeScope,
    },
    state = |state, options| {
        state.put(options.bridge);
        state.put(options.bake_scope);
    },
);

#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_bake_all(
    state: Rc<RefCell<OpState>>,
    #[serde] recipes: Vec<WithMeta<Recipe>>,
) -> Result<Vec<Artifact>, deno_core::error::AnyError> {
    let bridge = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeBridge>()
            .context("failed to get runtime bridge")?
            .clone()
    };
    let bake_scope = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<BakeScope>()
            .context("failed to get bake scope")?
            .clone()
    };

    let results = bridge.bake_all(recipes.clone(), bake_scope.clone()).await?;
    let results = results.into_iter().map(|result| result.value).collect();

    Ok(results)
}

#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_create_proxy(
    state: Rc<RefCell<OpState>>,
    #[serde] recipe: Recipe,
) -> Result<Recipe, deno_core::error::AnyError> {
    let bridge = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeBridge>()
            .context("failed to get runtime bridge")?
            .clone()
    };

    let result = bridge.create_proxy(recipe).await?;
    Ok(result)
}

// TODO: Return a Uint8Array instead of tick-encoding
#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_read_blob(
    state: Rc<RefCell<OpState>>,
    #[serde] blob_hash: BlobHash,
) -> Result<crate::encoding::TickEncode<Vec<u8>>, deno_core::error::AnyError> {
    let bridge = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeBridge>()
            .context("failed to get runtime bridge")?
            .clone()
    };

    let bytes = bridge.read_blob(blob_hash).await?;
    Ok(crate::encoding::TickEncode(bytes))
}

#[deno_core::op2(async)]
#[serde]
pub async fn op_brioche_get_static(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[serde] static_: StaticQuery,
) -> Result<bridge::GetStaticResult, deno_core::error::AnyError> {
    let bridge = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<RuntimeBridge>()
            .context("failed to get runtime bridge")?
            .clone()
    };

    let specifier: BriocheModuleSpecifier = url.parse()?;

    let recipe = bridge.get_static(specifier, static_).await?;
    Ok(recipe)
}
