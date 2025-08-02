use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, hash_map::Entry},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use anyhow::Context as _;
use bridge::{GetStaticOptions, RuntimeBridge};
use deno_core::OpState;
use specifier::BriocheModuleSpecifier;

use crate::bake::BakeScope;

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

/// A type representing a global JavaScript runtime platform, which is required
/// to create a JavaScript runtime.
///
/// Internally, this is an empty marker type, which indicates that the function
/// [`initialize_js_platform`] has been called appropriately (the actual
/// `v8::Platform` type is referenced globally).
#[derive(Debug, Clone, Copy)]
pub struct JsPlatform(());

/// Initialize the global JavaScript runtime platform. In most cases, this
/// function should be called from the program's main thread.
///
/// By default, the V8 JavaScript runtime will use the CPU's PKU feature if
/// available for additional memory protection. When enabled, the V8 platform
/// must be initialized on the main thread (or on an ancestor thread where V8
/// will run); otherwise, V8 operations may segfault.
///
/// This function should therefore be called on the main thread to set up
/// the V8 platform. The exception is if the `unsafe_use_unprotected_platform`
/// feature from `deno_core` is enabled, which makes this function safe to call
/// from arbitrary threads. This feature is enabled for `deno_core` during
/// tests, benchmarks, and examples.
///
/// The returned [`JsPlatform`] is a "marker" value, which is taken by various
/// functions before creating a `deno_core::JsRuntime` to ensure that
/// the V8 platform is initialized appropriately.
#[must_use]
pub fn initialize_js_platform() -> JsPlatform {
    deno_core::JsRuntime::init_platform(None, false);
    JsPlatform(())
}

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

        if let Entry::Vacant(entry) = self.sources.borrow_mut().entry(brioche_module_specifier) {
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

    fn resolve_module(
        &self,
        specifier: &str,
        referrer: &str,
        kind: &deno_core::ResolutionKind,
    ) -> anyhow::Result<deno_core::ModuleSpecifier> {
        if matches!(kind, deno_core::ResolutionKind::MainModule) {
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
}

impl deno_core::ModuleLoader for BriocheModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
    ) -> Result<deno_core::ModuleSpecifier, deno_core::error::ModuleLoaderError> {
        self.resolve_module(specifier, referrer, &kind)
            .map_err(|error| AnyError(error).into())
    }

    fn load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        _maybe_referrer: Option<&deno_core::ModuleSpecifier>,
        _is_dyn_import: bool,
        _requested_module_type: deno_core::RequestedModuleType,
    ) -> deno_core::ModuleLoadResponse {
        deno_core::ModuleLoadResponse::Sync(
            self.load_module_source(module_specifier)
                .map_err(|error| AnyError(error).into()),
        )
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
) -> Result<Vec<Artifact>, AnyError> {
    let bridge = {
        let state = state.try_borrow().map_err(AnyError::new)?;
        state
            .try_borrow::<RuntimeBridge>()
            .context("failed to get runtime bridge")?
            .clone()
    };
    let bake_scope = {
        let state = state.try_borrow().map_err(AnyError::new)?;
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
) -> Result<Recipe, AnyError> {
    let bridge = {
        let state = state.try_borrow().map_err(AnyError::new)?;
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
) -> Result<crate::encoding::TickEncode<Vec<u8>>, AnyError> {
    let bridge = {
        let state = state.try_borrow().map_err(AnyError::new)?;
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
    #[serde] options: GetStaticOptions,
) -> Result<bridge::GetStaticResult, AnyError> {
    let bridge = {
        let state = state.try_borrow().map_err(AnyError::new)?;
        state
            .try_borrow::<RuntimeBridge>()
            .context("failed to get runtime bridge")?
            .clone()
    };

    let specifier: BriocheModuleSpecifier = url.parse().map_err(AnyError::new)?;

    let recipe = bridge.get_static(specifier, options).await?;
    Ok(recipe)
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct AnyError(#[from] anyhow::Error);

impl AnyError {
    fn new<T>(value: T) -> Self
    where
        T: Into<anyhow::Error>,
    {
        Self(value.into())
    }
}

impl From<AnyError> for deno_error::JsErrorBox {
    fn from(value: AnyError) -> Self {
        Self::from_err(value)
    }
}

impl deno_error::JsErrorClass for AnyError {
    fn get_class(&self) -> Cow<'static, str> {
        Cow::Borrowed(deno_error::builtin_classes::GENERIC_ERROR)
    }

    fn get_message(&self) -> Cow<'static, str> {
        Cow::Owned(self.to_string())
    }

    fn get_additional_properties(&self) -> deno_error::AdditionalProperties {
        Box::new(std::iter::empty())
    }

    fn get_ref(&self) -> &(dyn ::std::error::Error + Send + Sync + 'static) {
        self
    }
}
