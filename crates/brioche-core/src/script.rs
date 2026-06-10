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
use deno_core::{OpState, v8};
use relative_path::PathExt as _;
use specifier::{BriocheModuleSpecifier, RUNTIME_SCHEME};

use crate::{bake::BakeScope, project::Projects, recipe::StackFrame};

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

/// Cap on stack frames retained per recipe meta.
pub const MAX_STACK_FRAMES: usize = 6;

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
    deno_core::JsRuntime::init_platform(None);
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
        requested_module_type: &deno_core::RequestedModuleType,
    ) -> Result<deno_core::ModuleSource, anyhow::Error> {
        let brioche_module_specifier: BriocheModuleSpecifier = module_specifier.try_into()?;
        let contents = self
            .bridge
            .read_specifier_contents(brioche_module_specifier.clone())?;

        match requested_module_type {
            deno_core::RequestedModuleType::Json => {
                let code = std::str::from_utf8(&contents)
                    .context("failed to parse JSON module contents as UTF-8 string")?;
                return Ok(deno_core::ModuleSource::new(
                    deno_core::ModuleType::Json,
                    deno_core::ModuleSourceCode::String(code.to_owned().into()),
                    module_specifier,
                    None,
                ));
            }
            deno_core::RequestedModuleType::Text => {
                let code = std::str::from_utf8(&contents)
                    .context("failed to parse text module contents as UTF-8 string")?;
                return Ok(deno_core::ModuleSource::new(
                    deno_core::ModuleType::Text,
                    deno_core::ModuleSourceCode::String(code.to_owned().into()),
                    module_specifier,
                    None,
                ));
            }
            deno_core::RequestedModuleType::Bytes => {
                let bytes = std::sync::Arc::<[u8]>::from(contents.as_slice());
                return Ok(deno_core::ModuleSource::new(
                    deno_core::ModuleType::Bytes,
                    deno_core::ModuleSourceCode::Bytes(bytes.into()),
                    module_specifier,
                    None,
                ));
            }
            deno_core::RequestedModuleType::None | deno_core::RequestedModuleType::Other(_) => {}
        }

        let code = std::str::from_utf8(&contents)
            .context("failed to parse module contents as UTF-8 string")?;

        // Serve prebuilt JS directly; transpile TypeScript sources.
        let media_type = deno_ast::MediaType::from_specifier(module_specifier);
        let (text, source_map) = if matches!(
            media_type,
            deno_ast::MediaType::JavaScript | deno_ast::MediaType::Mjs | deno_ast::MediaType::Cjs
        ) {
            (code.to_string(), Vec::new())
        } else {
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

            let deno_ast::EmittedSourceText { text, source_map } = transpiled.into_source();
            let source_map = source_map.context("source map not generated")?.into_bytes();
            (text, source_map)
        };

        if let Entry::Vacant(entry) = self.sources.borrow_mut().entry(brioche_module_specifier) {
            entry.insert(ModuleSource {
                source_contents: contents.clone(),
                source_map,
            });
        }

        // Only look up cached bytecode for the prebuilt runtime bundle.
        let code_cache_info = if module_specifier.scheme() == RUNTIME_SCHEME {
            let hash = code_cache_hash(text.as_bytes());
            let data = self.read_code_cache(hash).map(Cow::Owned);
            Some(deno_core::SourceCodeCacheInfo { hash, data })
        } else {
            None
        };

        Ok(deno_core::ModuleSource::new(
            deno_core::ModuleType::JavaScript,
            deno_core::ModuleSourceCode::String(text.into()),
            module_specifier,
            code_cache_info,
        ))
    }

    fn code_cache_path(&self, hash: u64) -> PathBuf {
        self.bridge
            .code_cache_dir()
            .join(format!("{hash:016x}.bin"))
    }

    fn read_code_cache(&self, hash: u64) -> Option<Vec<u8>> {
        std::fs::read(self.code_cache_path(hash)).ok()
    }

    /// Write V8 bytecode for a module to the cache, atomically. Failures are
    /// ignored.
    fn write_code_cache(&self, hash: u64, data: &[u8]) {
        let dir = self.bridge.code_cache_dir();
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = dir.join(format!("{hash:016x}.{}.tmp", ulid::Ulid::new()));
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, self.code_cache_path(hash));
        }
    }

    fn resolve_module(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
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
        self.resolve_module(specifier, referrer, kind)
            .map_err(|error| AnyError(error).into())
    }

    fn load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        _maybe_referrer: Option<&deno_core::ModuleLoadReferrer>,
        options: deno_core::ModuleLoadOptions,
    ) -> deno_core::ModuleLoadResponse {
        deno_core::ModuleLoadResponse::Sync(
            self.load_module_source(module_specifier, &options.requested_module_type)
                .map_err(|error| AnyError(error).into()),
        )
    }

    fn code_cache_ready(
        &self,
        module_specifier: deno_core::ModuleSpecifier,
        hash: u64,
        code_cache: &[u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
        if module_specifier.scheme() == RUNTIME_SCHEME {
            self.write_code_cache(hash, code_cache);
        }
        Box::pin(std::future::ready(()))
    }

    fn get_source_map(&self, file_name: &str) -> Option<Cow<'_, [u8]>> {
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

/// 64-bit blake3 digest of a module's source.
fn code_cache_hash(bytes: &[u8]) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(bytes);

    let mut digest = [0u8; 8];
    hasher.finalize_xof().fill(&mut digest);
    u64::from_le_bytes(digest)
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
        op_brioche_stack_frames_from_exception,
    ],
    options = {
        bridge: RuntimeBridge,
        bake_scope: BakeScope,
        projects: Projects,
    },
    state = |state, options| {
        state.put(options.bridge);
        state.put(options.bake_scope);
        state.put(options.projects);
    },
);

#[deno_core::op2]
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

#[deno_core::op2]
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
#[deno_core::op2]
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

#[deno_core::op2]
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

#[deno_core::op2(reentrant)]
#[serde]
fn op_brioche_stack_frames_from_exception<'a>(
    state: Rc<RefCell<OpState>>,
    scope: &mut v8::PinScope<'a, 'a>,
    exception: v8::Local<'a, v8::Value>,
) -> Vec<StackFrame> {
    let projects = state.borrow().try_borrow::<Projects>().cloned();
    drop(state);

    let error = deno_core::error::JsError::from_v8_exception(scope, exception);
    error
        .frames
        .into_iter()
        .take(MAX_STACK_FRAMES)
        .map(|frame| enrich_stack_frame(projects.as_ref(), frame))
        .collect()
}

fn enrich_stack_frame(
    projects: Option<&Projects>,
    frame: deno_core::error::JsStackFrame,
) -> StackFrame {
    let (project_name, module_path) = projects
        .and_then(|p| resolve_frame_project_context(p, frame.file_name.as_deref()))
        .unzip();
    StackFrame {
        file_name: frame.file_name,
        line_number: frame.line_number,
        column_number: frame.column_number,
        project_name,
        module_path,
    }
}

fn resolve_frame_project_context(
    projects: &Projects,
    file_name: Option<&str>,
) -> Option<(String, String)> {
    let file_name = file_name?;
    let url: url::Url = file_name.parse().ok()?;
    let specifier = BriocheModuleSpecifier::try_from(&url).ok()?;
    let path = match specifier {
        BriocheModuleSpecifier::File { path } => path,
        BriocheModuleSpecifier::Runtime { .. } => return None,
    };

    let project_hash = projects.find_containing_project(&path).ok().flatten()?;
    let project_root = projects
        .find_containing_project_root(&path, project_hash)
        .ok()?;
    let project = projects.project(project_hash).ok()?;

    let project_name = project.definition.name.clone().or_else(|| {
        project_root
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
    })?;
    let module_path = path.relative_to(&project_root).ok()?.to_string();

    Some((project_name, module_path))
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
