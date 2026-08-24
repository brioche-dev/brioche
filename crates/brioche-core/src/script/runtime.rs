use std::{borrow::Cow, collections::HashMap, rc::Rc, sync::Arc};

use futures::TryFutureExt as _;

use crate::{
    path::AbsolutePath,
    project::{ModuleRef, ProjectRef},
    recipe::RecipeRef,
    script::specifier::{ImportSpecifier, ModuleSpecifier},
};

pub mod deserialize;

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
        let brioche = brioche.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        std::thread::spawn(move || {
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

            runtime.block_on(async move {
                let mut bridge = JsRuntimeBridge::new(brioche);
                while let Some(message) = rx.recv().await {
                    bridge.handle_message(message).await;
                }
            });
        });

        let (pong, pong_rx) = tokio::sync::oneshot::channel();
        tx.send(JsRuntimeMessage::Ping { pong })
            .map_err(|_| JsRuntimeError::ChannelClosed)?;
        pong_rx.await.map_err(|_| JsRuntimeError::NoResponse)?;

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
            .map_err(|_| EvaluateError::SendError)?;
        result_rx.await?
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
    fn new(brioche: crate::Brioche) -> Self {
        let (worker_tx, mut worker_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::task::spawn({
            let brioche = brioche.clone();
            async move {
                while let Some(message) = worker_rx.recv().await {
                    match message {
                        JsRuntimeBridgeWorkerMessage::ResolveImportSpecifier {
                            specifier,
                            referrer,
                            result_tx,
                        } => {
                            let brioche = brioche.read().await;
                            let result = crate::script::specifier::resolve_import_specifier(
                                &brioche, &specifier, &referrer,
                            );
                            let _ = result_tx.send(result);
                        }
                    }
                }
            }
        });

        let module_loader = JsModuleLoader {
            brioche: brioche.clone(),
            worker_tx,
        };
        let js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            module_loader: Some(Rc::new(module_loader)),
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

    async fn load_module(&mut self, module_ref: ModuleRef) -> Result<usize, EvaluateError> {
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
            root_module_ref = brioche
                .projects
                .get_root_module(project_ref)
                .ok_or_else(|| {
                    let project_path = brioche.projects.local_project_path(project_ref);
                    EvaluateError::NoRootModule {
                        path: project_path.clone(),
                        project_ref,
                    }
                })?;
            root_module_path = brioche.projects.local_module_path(root_module_ref);
        }

        // Load the module
        let module_id = self.load_module(root_module_ref).await?;
        let module_namespace = self.js_runtime.get_module_namespace(module_id)?;

        // Get the export by name
        let (export_value, module_namespace) = {
            deno_core::scope!(js_scope, self.js_runtime);
            deno_core::v8::tc_scope!(let js_scope, js_scope);

            let module_namespace = deno_core::v8::Local::new(js_scope, module_namespace);
            let export_key = deno_core::v8::String::new(js_scope, &export)
                .ok_or_else(|| EvaluateError::InvalidJsString(export.clone().into()))?;
            let export_value = module_namespace
                .get(js_scope, export_key.into())
                .ok_or_else(|| EvaluateError::NoExport {
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

        let mut brioche = self.brioche.write().await;
        let export_value = deserialize::ValueScope::new(
            export_value,
            deserialize::ValuePath::top_level(root_module_ref, root_module_path, export),
        );
        let recipe = deserialize::deserialize_recipe(
            &mut brioche.recipes,
            &mut self.js_runtime,
            export_value,
            &module_namespace,
            &mut self.cached_recipes,
        )
        .await
        .map_err(|error| EvaluateError::DeserializeError(Box::new(error)))?;

        Ok(recipe)
    }
}

#[derive(Clone)]
pub struct JsValueHandle(Arc<usize>);

pub struct JsContext<'a, 'p, 'scope, 'obj, 's> {
    js_scope: &'a deno_core::v8::PinnedRef<
        'p,
        deno_core::v8::TryCatch<'scope, 'obj, deno_core::v8::HandleScope<'s>>,
    >,
    values: &'a mut HashMap<
        usize,
        (
            std::sync::Weak<usize>,
            deno_core::v8::Global<deno_core::v8::Value>,
        ),
    >,
    #[expect(unused)]
    next_value_id: &'a mut usize,
}

impl JsContext<'_, '_, '_, '_, '_> {
    pub fn type_repr(&mut self, value: &JsValueHandle) -> String {
        let (_, value) = &self.values[&value.0];
        let value = deno_core::v8::Local::new(self.js_scope, value);
        value.type_repr().to_string()
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
}

struct JsModuleLoader {
    brioche: crate::Brioche,
    worker_tx: tokio::sync::mpsc::UnboundedSender<JsRuntimeBridgeWorkerMessage>,
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
        specifier_url: deno_core::ModuleSpecifier,
        specifier: crate::script::specifier::ModuleSpecifier,
        referrer: Option<deno_core::ModuleLoadReferrer>,
        options: deno_core::ModuleLoadOptions,
    ) -> Result<deno_core::ModuleSource, LoadModuleError> {
        let brioche = brioche.read().await;
        let code = match &specifier {
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
                // TODO: Use an Arc for sources
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

                deno_core::ModuleSourceCode::String(source.clone().into())
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
            module_specifier.clone(),
            specifier,
            maybe_referrer.cloned(),
            options,
        )
        .map_err(deno_core::error::ModuleLoaderError::from_err);
        deno_core::ModuleLoadResponse::Async(Box::pin(module_fut))
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

#[derive(Debug, thiserror::Error)]
pub enum JsRuntimeError {
    #[error("JS runtime channel closed unexpectedly")]
    ChannelClosed,

    #[error("JS runtime task did not send a response")]
    NoResponse,
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
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluateError {
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

    #[error("invalid enum variant '{got}', expected one of {expected:?}")]
    InvalidEnumVariant {
        expected: Vec<&'static str>,
        got: String,
    },

    #[error("failed to send eval message to channel")]
    SendError,

    #[error("failed to get eval result from channel: {0}")]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error(transparent)]
    ModuleSpecifierToUrlError(#[from] crate::script::specifier::ModuleSpecifierToUrlError),

    #[error(transparent)]
    JsError(#[from] Box<deno_core::error::JsError>),

    #[error(transparent)]
    DenoCoreError(#[from] deno_core::error::CoreError),

    #[error("expected type {expected}, got {actual}")]
    TypeError {
        expected: Cow<'static, str>,
        actual: Cow<'static, str>,
    },

    #[error(transparent)]
    TickEncodingDecodeError(#[from] tick_encoding::DecodeError),

    #[error(transparent)]
    DeserializeError(Box<deserialize::DeserializeError>),
}

impl From<std::convert::Infallible> for EvaluateError {
    fn from(value: std::convert::Infallible) -> Self {
        match value {}
    }
}
