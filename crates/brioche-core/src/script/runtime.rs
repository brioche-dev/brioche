use std::{borrow::Cow, collections::HashMap, rc::Rc, sync::Arc};

use bstr::ByteSlice as _;
use futures::TryFutureExt as _;

use crate::{
    path::AbsolutePath,
    project::{ModuleRef, ProjectRef},
    recipe::RecipeRef,
    script::specifier::{ImportSpecifier, ModuleSpecifier},
};

pub mod deserialize;
mod js_extension;

type JsRuntimeBridgeWorkerMessageSender =
    tokio::sync::mpsc::UnboundedSender<JsRuntimeBridgeWorkerMessage>;

const MODULE_RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub struct JsRuntime {
    tx: tokio::sync::mpsc::UnboundedSender<JsRuntimeMessage>,
    _platform: JsPlatform,
}

impl JsRuntime {
    pub async fn new(
        brioche: &crate::Brioche,
        platform: JsPlatform,
    ) -> Result<Self, JsRuntimeError> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (worker_tx, mut worker_rx) = tokio::sync::mpsc::unbounded_channel();

        std::thread::spawn({
            let brioche = brioche.clone();
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build_local(tokio::runtime::LocalOptions::default());
                let runtime = match runtime {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!("failed to build Tokio runtime for JS runtime: {error:#}");
                        return;
                    }
                };

                runtime.block_on({
                    let brioche = brioche.clone();
                    async move {
                        let mut bridge = JsRuntimeBridge::new(brioche, worker_tx);
                        while let Some(message) = rx.recv().await {
                            bridge.handle_message(message).await;
                        }
                    }
                });
            }
        });

        tokio::spawn({
            let brioche = brioche.clone();
            async move {
                let worker = JsRuntimeBridgeWorker::new(brioche);
                while let Some(message) = worker_rx.recv().await {
                    worker.handle_message(message).await;
                }
            }
        });

        let (pong, pong_rx) = tokio::sync::oneshot::channel();
        tx.send(JsRuntimeMessage::Ping { pong })
            .map_err(|_| JsRuntimeError::SendChannelClosed)?;
        pong_rx
            .await
            .map_err(|_| JsRuntimeError::RecvChannelClosed)?;

        Ok(Self {
            tx,
            _platform: platform,
        })
    }

    pub async fn get_recipe_export(
        &self,
        project_ref: ProjectRef,
        export: &str,
    ) -> Result<RecipeRef, EvaluateError> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(JsRuntimeMessage::GetRecipeExport {
                project_ref,
                export: export.to_string(),
                result_tx,
            })
            .map_err(|_| JsRuntimeError::SendChannelClosed)?;
        result_rx
            .await
            .map_err(|_| JsRuntimeError::RecvChannelClosed)?
    }
}

enum JsRuntimeMessage {
    Ping {
        pong: tokio::sync::oneshot::Sender<()>,
    },
    GetRecipeExport {
        project_ref: ProjectRef,
        export: String,
        result_tx: tokio::sync::oneshot::Sender<Result<RecipeRef, EvaluateError>>,
    },
}

struct JsRuntimeBridge {
    brioche: crate::Brioche,
    js_runtime: deno_core::JsRuntime,
    module_ids: HashMap<ModuleRef, usize>,
    cached_recipes: HashMap<deno_core::v8::Global<deno_core::v8::Value>, RecipeRef>,
}

impl JsRuntimeBridge {
    fn new(brioche: crate::Brioche, worker_tx: JsRuntimeBridgeWorkerMessageSender) -> Self {
        let module_loader = JsModuleLoader {
            brioche: brioche.clone(),
            source_maps: Arc::new(std::sync::RwLock::new(HashMap::new())),
            worker_tx: worker_tx.clone(),
        };
        let js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            module_loader: Some(Rc::new(module_loader)),
            extensions: vec![js_extension::brioche_extension::init(worker_tx)],
            ..Default::default()
        });

        Self {
            brioche,
            js_runtime,
            module_ids: HashMap::new(),
            cached_recipes: HashMap::new(),
        }
    }

    async fn handle_message(&mut self, message: JsRuntimeMessage) {
        match message {
            JsRuntimeMessage::Ping { pong } => {
                let _ = pong.send(());
            }
            JsRuntimeMessage::GetRecipeExport {
                project_ref,
                export,
                result_tx,
            } => {
                let result = self.get_recipe_export(project_ref, export).await;
                let _ = result_tx.send(result);
            }
        }
    }

    async fn load_module(&mut self, module_ref: ModuleRef) -> Result<usize, JsRuntimeError> {
        let module_id = match self.module_ids.entry(module_ref) {
            std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let specifier = {
                    let brioche = self.brioche.read().await;
                    let path = brioche.projects.local_module_path(module_ref);
                    crate::script::specifier::ModuleSpecifier::File { path }
                };
                let specifier = specifier.try_into()?;

                // Load and evaluate the main module
                let module_id = self.js_runtime.load_main_es_module(&specifier).await?;
                let result = self.js_runtime.mod_evaluate(module_id);
                self.js_runtime
                    .run_event_loop(deno_core::PollEventLoopOptions::default())
                    .await?;
                result.await?;

                *entry.insert(module_id)
            }
        };
        Ok(module_id)
    }

    async fn get_recipe_export(
        &mut self,
        project_ref: ProjectRef,
        export: String,
    ) -> Result<RecipeRef, EvaluateError> {
        let root_module_ref;
        let root_module_path;

        // Get the root module for the project
        {
            let brioche = self.brioche.read().await;
            root_module_ref = brioche.projects.root_module(project_ref).ok_or_else(|| {
                let project_path = brioche.projects.local_project_path(project_ref);
                JsRuntimeError::NoRootModule {
                    path: project_path.clone(),
                    project_ref,
                }
            })?;
            root_module_path = brioche.projects.local_module_path(root_module_ref);
        }

        // Load the module
        let module_id = self.load_module(root_module_ref).await?;
        let module_namespace = self
            .js_runtime
            .get_module_namespace(module_id)
            .map_err(JsRuntimeError::from)?;

        // Get the export by name
        let (export_value, module_namespace) = {
            deno_core::scope!(js_scope, self.js_runtime);
            deno_core::v8::tc_scope!(let js_scope, js_scope);

            let module_namespace = deno_core::v8::Local::new(js_scope, module_namespace);
            let export_key = deno_core::v8::String::new(js_scope, &export)
                .ok_or_else(|| JsRuntimeError::InvalidJsString(export.clone().into()))?;
            let export_value = module_namespace
                .get(js_scope, export_key.into())
                .ok_or_else(|| JsRuntimeError::NoExport {
                    module_ref: root_module_ref,
                    module_path: root_module_path.clone(),
                    export: export.clone(),
                })?;
            let export_value = deno_core::v8::Global::new(js_scope, export_value);

            let module_namespace =
                deno_core::v8::Local::<deno_core::v8::Value>::from(module_namespace);
            let module_namespace = deno_core::v8::Global::new(js_scope, module_namespace);

            (export_value, module_namespace)
        };

        let export_value = deserialize::ValueScope::new(
            export_value,
            deserialize::ValuePath::top_level(root_module_ref, root_module_path, export),
        );
        let recipe = deserialize::deserialize_recipe(
            &self.brioche,
            &mut self.js_runtime,
            export_value,
            &module_namespace,
            &mut self.cached_recipes,
        )
        .await?;

        Ok(recipe)
    }
}

struct JsRuntimeBridgeWorker {
    brioche: crate::Brioche,
}

impl JsRuntimeBridgeWorker {
    const fn new(brioche: crate::Brioche) -> Self {
        Self { brioche }
    }

    async fn handle_message(&self, message: JsRuntimeBridgeWorkerMessage) {
        match message {
            JsRuntimeBridgeWorkerMessage::ResolveImportSpecifier {
                specifier,
                referrer,
                result_tx,
            } => {
                let brioche = self.brioche.read().await;
                let result = crate::script::specifier::resolve_import_specifier(
                    &brioche, &specifier, &referrer,
                );
                let _ = result_tx.send(result);
            }
            JsRuntimeBridgeWorkerMessage::EnrichStackFrames { frames, result_tx } => {
                let brioche = self.brioche.read().await;
                let frames = frames
                    .into_iter()
                    .map(|frame| enrich_stack_frame(brioche.projects(), frame))
                    .collect();
                let _ = result_tx.send(frames);
            }
        }
    }
}

enum JsRuntimeBridgeWorkerMessage {
    ResolveImportSpecifier {
        specifier: ImportSpecifier,
        referrer: ModuleSpecifier,
        result_tx: std::sync::mpsc::Sender<
            Result<ModuleSpecifier, crate::script::specifier::ResolveSpecifierError>,
        >,
    },
    EnrichStackFrames {
        frames: Vec<deno_core::error::JsStackFrame>,
        result_tx: std::sync::mpsc::Sender<Vec<StackFrame>>,
    },
}

struct JsModuleLoader {
    brioche: crate::Brioche,
    source_maps:
        Arc<std::sync::RwLock<HashMap<crate::script::specifier::ModuleSpecifier, Arc<str>>>>,
    worker_tx: JsRuntimeBridgeWorkerMessageSender,
}

impl JsModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: &deno_core::ResolutionKind,
        timeout: Option<std::time::Duration>,
    ) -> Result<deno_core::ModuleSpecifier, ResolveModuleError> {
        if matches!(kind, deno_core::ResolutionKind::MainModule) {
            let resolved = specifier.parse().map_err(|error| {
                crate::script::specifier::ModuleSpecifierParseError::UrlParseError {
                    error,
                    url: specifier.to_string(),
                }
            })?;
            tracing::debug!(%specifier, %referrer, %resolved, "resolved main module");
            return Ok(resolved);
        }

        let referrer: ModuleSpecifier = referrer.parse()?;
        let specifier: ImportSpecifier = specifier
            .parse()
            .map_err(|error| -> ResolveModuleError { match error {} })?;

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        self.worker_tx
            .send(JsRuntimeBridgeWorkerMessage::ResolveImportSpecifier {
                specifier,
                referrer,
                result_tx,
            })
            .map_err(|_| ResolveModuleError::Send)?;
        let resolved = if let Some(timeout) = timeout {
            result_rx
                .recv_timeout(timeout)
                .map_err(|error| ResolveModuleError::from_recv_timeout(error, timeout))??
        } else {
            result_rx.recv()??
        };
        let resolved = resolved.try_into()?;
        Ok(resolved)
    }

    async fn load(
        brioche: crate::Brioche,
        source_maps: Arc<
            std::sync::RwLock<HashMap<crate::script::specifier::ModuleSpecifier, Arc<str>>>,
        >,
        specifier_url: deno_core::ModuleSpecifier,
        specifier: crate::script::specifier::ModuleSpecifier,
        referrer: Option<deno_core::ModuleLoadReferrer>,
        options: deno_core::ModuleLoadOptions,
    ) -> Result<deno_core::ModuleSource, LoadModuleError> {
        let brioche = brioche.read().await;
        let source = match &specifier {
            ModuleSpecifier::Runtime {
                subpath_components: _,
            } => {
                // TODO: Implement loading runtime modules
                return Err(LoadModuleError::NotFound {
                    specifier,
                    referrer,
                });
            }
            ModuleSpecifier::File { path } => {
                // TODO: Support non-module imports
                let module_ref = brioche.projects.module_by_path(path).ok_or_else(|| {
                    LoadModuleError::NotFound {
                        specifier: specifier.clone(),
                        referrer: referrer.clone(),
                    }
                })?;
                let module = brioche.projects.module(module_ref);
                let source = module.source.as_ref().map_err(|error| {
                    LoadModuleError::ProjectLoadModuleError {
                        specifier: specifier.clone(),
                        referrer: referrer.clone(),
                        error: error.clone(),
                    }
                })?;

                source.clone()
            }
        };
        let module_type = match options.requested_module_type {
            deno_core::RequestedModuleType::None => deno_core::ModuleType::JavaScript,
            deno_core::RequestedModuleType::Json => deno_core::ModuleType::Json,
            deno_core::RequestedModuleType::Text => deno_core::ModuleType::Text,
            deno_core::RequestedModuleType::Bytes => deno_core::ModuleType::Bytes,
            deno_core::RequestedModuleType::Other(other) => {
                deno_core::ModuleType::Other(other.clone())
            }
        };

        // Return JavaScript verbatim, and transpile TypeScript
        let (source, source_map) =
            if should_transpile_typescript(&module_type, &specifier, &specifier_url) {
                let parsed = deno_ast::parse_module(deno_ast::ParseParams {
                    specifier: specifier_url.clone(),
                    text: source,
                    media_type: deno_ast::MediaType::TypeScript,
                    capture_tokens: false,
                    scope_analysis: false,
                    maybe_syntax: None,
                })
                .map_err(|error| LoadModuleError::ParseError {
                    specifier: specifier.clone(),
                    referrer: referrer.clone(),
                    error,
                })?;
                let transpiled = parsed
                    .transpile(
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
                    )
                    .map_err(|error| LoadModuleError::TranspileError {
                        specifier: specifier.clone(),
                        referrer: referrer.clone(),
                        error,
                    })?;

                let deno_ast::EmittedSourceText { text, source_map } = transpiled.into_source();
                let text = Arc::<str>::from(text);
                (text, source_map)
            } else {
                (source, None)
            };
        let code = deno_core::ModuleSourceCode::String(source.into());

        if let Some(source_map) = source_map {
            let mut source_maps = source_maps.write().unwrap();
            source_maps.insert(specifier, source_map.into());
        }

        Ok(deno_core::ModuleSource::new(
            module_type,
            code,
            &specifier_url,
            None,
        ))
    }
}

impl deno_core::ModuleLoader for JsModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
    ) -> Result<deno_core::ModuleSpecifier, deno_core::error::ModuleLoaderError> {
        self.resolve(specifier, referrer, &kind, Some(MODULE_RESOLVE_TIMEOUT))
            .map_err(deno_core::error::ModuleLoaderError::from_err)
    }

    fn load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        maybe_referrer: Option<&deno_core::ModuleLoadReferrer>,
        options: deno_core::ModuleLoadOptions,
    ) -> deno_core::ModuleLoadResponse {
        let specifier = crate::script::specifier::ModuleSpecifier::try_from(module_specifier);
        let specifier = match specifier {
            Ok(specifier) => specifier,
            Err(error) => {
                return deno_core::ModuleLoadResponse::Sync(Err(
                    deno_core::error::ModuleLoaderError::from_err(LoadModuleError::from(error)),
                ));
            }
        };
        let module_fut = Self::load(
            self.brioche.clone(),
            self.source_maps.clone(),
            module_specifier.clone(),
            specifier,
            maybe_referrer.cloned(),
            options,
        )
        .map_err(deno_core::error::ModuleLoaderError::from_err);
        deno_core::ModuleLoadResponse::Async(Box::pin(module_fut))
    }

    fn source_map_source_exists(&self, source_url: &str) -> Option<bool> {
        let Ok(specifier_url) = source_url.parse::<url::Url>() else {
            return None;
        };
        let specifier = crate::script::specifier::ModuleSpecifier::try_from(&specifier_url);
        let Ok(specifier) = specifier else {
            return None;
        };

        let source_maps = self.source_maps.read().unwrap();
        Some(source_maps.contains_key(&specifier))
    }

    fn get_source_map(&self, file_name: &str) -> Option<Cow<'_, [u8]>> {
        let Ok(specifier_url) = file_name.parse::<url::Url>() else {
            return None;
        };
        let specifier = crate::script::specifier::ModuleSpecifier::try_from(&specifier_url);
        let Ok(specifier) = specifier else {
            return None;
        };

        let source_maps = self.source_maps.read().unwrap();
        let source_map = source_maps.get(&specifier);
        source_map.map(|source_map| source_map.as_bytes().to_vec().into())
    }
}

/// A type representing a global JavaScript runtime platform, which is required
/// to create a JavaScript runtime.
///
/// Internally, this is an empty marker type, which indicates that the function
/// [`initialize_js_platform`] has been called appropriately (the actual
/// `v8::Platform` type is referenced globally).
#[derive(Debug, Clone, Copy)]
pub struct JsPlatform(());

#[must_use]
pub fn initialize_js_platform() -> JsPlatform {
    deno_core::JsRuntime::init_platform(None);
    JsPlatform(())
}

fn enrich_stack_frame(
    projects: &crate::project::Projects,
    frame: deno_core::error::JsStackFrame,
) -> StackFrame {
    let (project_name, module_path) =
        resolve_frame_project_context(projects, frame.file_name.as_deref()).unzip();
    StackFrame {
        file_name: frame.file_name,
        line_number: frame.line_number,
        column_number: frame.column_number,
        project_name,
        module_path,
    }
}

fn resolve_frame_project_context(
    projects: &crate::project::Projects,
    file_name: Option<&str>,
) -> Option<(String, String)> {
    let file_name = file_name?;
    let url: url::Url = file_name.parse().ok()?;
    let specifier = ModuleSpecifier::try_from(&url).ok()?;
    let path = match specifier {
        ModuleSpecifier::File { path } => path,
        ModuleSpecifier::Runtime { .. } => return None,
    };

    let module_ref = projects.module_by_path(&path)?;
    let (project_ref, module_subpath) = projects.project_by_module(module_ref)?;

    let project = projects.project(*project_ref);

    let project_name = project.definition.name.clone().or_else(|| {
        Some(
            projects
                .local_project_path(*project_ref)
                .filename()?
                .to_str_lossy()
                .into_owned(),
        )
    })?;
    let module_subpath = module_subpath.to_string();

    Some((project_name, module_subpath))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub file_name: Option<String>,
    pub line_number: Option<i64>,
    pub column_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_path: Option<String>,
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path: std::borrow::Cow<'_, str> = match (&self.project_name, &self.module_path) {
            (Some(project), Some(module)) => std::borrow::Cow::Owned(format!("{project}/{module}")),
            (None, Some(module)) => std::borrow::Cow::Borrowed(module),
            _ => self.file_name.as_deref().map_or(
                std::borrow::Cow::Borrowed("<unknown>"),
                std::borrow::Cow::Borrowed,
            ),
        };
        match (self.line_number, self.column_number) {
            (Some(line), Some(column)) => write!(f, "{path}:{line}:{column}"),
            (Some(line), None) => write!(f, "{path}:{line}"),
            (None, _) => write!(f, "{path}"),
        }
    }
}

fn should_transpile_typescript(
    module_type: &deno_core::ModuleType,
    specifier: &ModuleSpecifier,
    specifier_url: &deno_core::ModuleSpecifier,
) -> bool {
    if !matches!(module_type, deno_core::ModuleType::JavaScript) {
        return false;
    }

    match specifier {
        ModuleSpecifier::File { .. } => true,
        ModuleSpecifier::Runtime { .. } => {
            let media_type = deno_ast::MediaType::from_specifier(specifier_url);
            match media_type {
                deno_ast::MediaType::TypeScript
                | deno_ast::MediaType::Mts
                | deno_ast::MediaType::Cts
                | deno_ast::MediaType::Dts
                | deno_ast::MediaType::Dmts
                | deno_ast::MediaType::Dcts
                | deno_ast::MediaType::Tsx => true,
                deno_ast::MediaType::Jsx
                | deno_ast::MediaType::JavaScript
                | deno_ast::MediaType::Mjs
                | deno_ast::MediaType::Cjs
                | deno_ast::MediaType::Css
                | deno_ast::MediaType::Json
                | deno_ast::MediaType::Jsonc
                | deno_ast::MediaType::Json5
                | deno_ast::MediaType::Html
                | deno_ast::MediaType::Markdown
                | deno_ast::MediaType::Sql
                | deno_ast::MediaType::Wasm
                | deno_ast::MediaType::SourceMap
                | deno_ast::MediaType::Unknown => false,
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JsRuntimeError {
    #[error("project '{path}' does not have a root module")]
    NoRootModule {
        path: AbsolutePath,
        project_ref: ProjectRef,
    },

    #[error("invalid JS string value: {0:?}")]
    InvalidJsString(Cow<'static, str>),

    #[error("module '{module_path}' does not have an export named '{export}'")]
    NoExport {
        module_ref: ModuleRef,
        module_path: AbsolutePath,
        export: String,
    },

    #[error("unknown error while evaluating JS: {reason}")]
    UnknownEvalError { reason: Cow<'static, str> },

    #[error("missing field")]
    MissingField,

    #[error("failed to send eval message to channel: channel closed")]
    SendChannelClosed,

    #[error("failed to get eval result from channel: channel closed")]
    RecvChannelClosed,

    #[error("invalid enum variant '{got}', expected one of {expected:?}")]
    InvalidEnumVariant {
        expected: Vec<&'static str>,
        got: String,
    },

    #[error("expected type {expected}, got {actual}")]
    TypeError {
        expected: Cow<'static, str>,
        actual: Cow<'static, str>,
    },

    #[error(transparent)]
    ModuleSpecifierToUrlError(#[from] crate::script::specifier::ModuleSpecifierToUrlError),

    #[error(transparent)]
    JsError(#[from] Box<deno_core::error::JsError>),

    #[error(transparent)]
    DenoCoreError(#[from] deno_core::error::CoreError),

    #[error(transparent)]
    TickEncodingDecodeError(#[from] tick_encoding::DecodeError),
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class(generic)]
pub enum ResolveModuleError {
    #[error(transparent)]
    ModuleSpecifierParse(#[from] crate::script::specifier::ModuleSpecifierParseError),

    #[error(transparent)]
    ModuleSpecifierToUrl(#[from] crate::script::specifier::ModuleSpecifierToUrlError),

    #[error(transparent)]
    ResolveSpecifier(#[from] Box<crate::script::specifier::ResolveSpecifierError>),

    #[error("failed to send message to worker channel")]
    Send,

    #[error("timed out after {} while waiting for message from worker channel", humantime::format_duration(*.duration))]
    RecvTimeout { duration: std::time::Duration },

    #[error("error receiving message from worker channel: {0}")]
    Recv(#[from] std::sync::mpsc::RecvError),
}

impl ResolveModuleError {
    const fn from_recv_timeout(
        error: std::sync::mpsc::RecvTimeoutError,
        duration: std::time::Duration,
    ) -> Self {
        match error {
            std::sync::mpsc::RecvTimeoutError::Timeout => Self::RecvTimeout { duration },
            std::sync::mpsc::RecvTimeoutError::Disconnected => {
                Self::Recv(std::sync::mpsc::RecvError)
            }
        }
    }
}

impl From<crate::script::specifier::ResolveSpecifierError> for ResolveModuleError {
    fn from(error: crate::script::specifier::ResolveSpecifierError) -> Self {
        Self::ResolveSpecifier(Box::new(error))
    }
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class(generic)]
pub enum LoadModuleError {
    #[error(transparent)]
    ModuleSpecifierParse(#[from] crate::script::specifier::ModuleSpecifierParseError),

    #[error(transparent)]
    ModuleSpecifierToUrl(#[from] crate::script::specifier::ModuleSpecifierToUrlError),

    #[error(
        "module '{specifier}' not found{}",
        referrer.as_ref().map_or_else(
            String::new,
            |referrer| format!(" (referred by {}:{}:{})", referrer.specifier, referrer.line_number, referrer.column_number),
        )
    )]
    NotFound {
        specifier: crate::script::specifier::ModuleSpecifier,
        referrer: Option<deno_core::ModuleLoadReferrer>,
    },

    #[error(
        "failed to load module '{specifier}'{}: {error}",
        referrer.as_ref().map_or_else(
            String::new,
            |referrer| format!(" (referred by {}:{}:{})", referrer.specifier, referrer.line_number, referrer.column_number),
        )
    )]
    ProjectLoadModuleError {
        specifier: crate::script::specifier::ModuleSpecifier,
        referrer: Option<deno_core::ModuleLoadReferrer>,
        #[source]
        error: crate::project::load::LoadModuleError,
    },

    #[error(
        "failed to parse module '{specifier}'{}: {error}",
        referrer.as_ref().map_or_else(
            String::new,
            |referrer| format!(" (referred by {}:{}:{})", referrer.specifier, referrer.line_number, referrer.column_number),
        )
    )]
    ParseError {
        specifier: crate::script::specifier::ModuleSpecifier,
        referrer: Option<deno_core::ModuleLoadReferrer>,
        #[source]
        error: deno_ast::ParseDiagnostic,
    },

    #[error(
        "failed to transpile module '{specifier}'{}: {error}",
        referrer.as_ref().map_or_else(
            String::new,
            |referrer| format!(" (referred by {}:{}:{})", referrer.specifier, referrer.line_number, referrer.column_number),
        )
    )]
    TranspileError {
        specifier: crate::script::specifier::ModuleSpecifier,
        referrer: Option<deno_core::ModuleLoadReferrer>,
        #[source]
        error: deno_ast::TranspileError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluateError {
    #[error(transparent)]
    Runtime(#[from] JsRuntimeError),

    #[error(transparent)]
    Deserialize(#[from] deserialize::DeserializeError),
}

impl From<std::convert::Infallible> for EvaluateError {
    fn from(value: std::convert::Infallible) -> Self {
        match value {}
    }
}
