use std::{borrow::Cow, collections::HashMap, rc::Rc, sync::Arc};

use futures::TryFutureExt as _;

use crate::{
    path::AbsolutePath,
    project::{ModuleRef, ProjectRef},
    recipe::{Recipe, RecipeRef},
    script::specifier::{ImportSpecifier, ModuleSpecifier},
};

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
            let export_key = v8_string_inner(js_scope, &export)?;
            let export_value = module_namespace
                .get(js_scope, export_key.into())
                .ok_or_else(|| EvaluateError::NoExport {
                    module_ref: root_module_ref,
                    module_path: root_module_path,
                    export,
                })?;
            let export_value = deno_core::v8::Global::new(js_scope, export_value);

            let module_namespace =
                deno_core::v8::Local::<deno_core::v8::Value>::from(module_namespace);
            let module_namespace = deno_core::v8::Global::new(js_scope, module_namespace);

            (export_value, module_namespace)
        };

        let mut brioche = self.brioche.write().await;
        let recipe = eval_recipe(
            &mut brioche.recipes,
            &mut self.js_runtime,
            &export_value,
            &module_namespace,
            &mut self.cached_recipes,
            true,
        )
        .await?;

        Ok(recipe)
    }
}

#[expect(clippy::mutable_key_type)]
async fn eval_recipe(
    recipes: &mut crate::recipe::Recipes,
    js_runtime: &mut deno_core::JsRuntime,
    value: &deno_core::v8::Global<deno_core::v8::Value>,
    module_namespace: &deno_core::v8::Global<deno_core::v8::Value>,
    cached_recipes: &mut HashMap<deno_core::v8::Global<deno_core::v8::Value>, RecipeRef>,
    top_level: bool,
) -> Result<RecipeRef, EvaluateError> {
    let mut equivalent_values = vec![];

    if let Some(recipe) = cached_recipes.get(value) {
        return Ok(*recipe);
    }
    equivalent_values.push(value.clone());

    // If the value is a function, call it. If it returns a promise, resolve it.
    let function: Result<deno_core::v8::Global<deno_core::v8::Function>, _> =
        v8_cast(js_runtime, value);
    let value = if let Ok(function) = function {
        let value = v8_call(js_runtime, function, Some(module_namespace), [])?;
        let value = js_runtime.resolve(value).await?;

        if let Some(recipe) = cached_recipes.get(&value).copied() {
            cached_recipes.extend(equivalent_values.into_iter().map(|value| (value, recipe)));
            return Ok(recipe);
        }
        equivalent_values.push(value.clone());

        value
    } else {
        value.clone()
    };
    let value_object = v8_cast::<_, deno_core::v8::Object>(js_runtime, &value)?;

    // If the the value has a `briocheSerialize` method, call and resolve it.
    let brioche_serialize = v8_get_nullish(js_runtime, &value_object, "briocheSerialize")?;
    let value = if let Some(brioche_serialize) = brioche_serialize {
        let brioche_serialize =
            v8_cast::<_, deno_core::v8::Function>(js_runtime, &brioche_serialize)?;
        let value = v8_call(js_runtime, brioche_serialize, Some(&value), [])?;
        let value = js_runtime.resolve(value).await?;

        if let Some(recipe) = cached_recipes.get(&value).copied() {
            cached_recipes.extend(equivalent_values.into_iter().map(|value| (value, recipe)));
            return Ok(recipe);
        }
        equivalent_values.push(value.clone());

        v8_cast::<_, deno_core::v8::Object>(js_runtime, &value)?
    } else if top_level {
        return Err(EvaluateError::InvalidRecipe {
            reason: "recipe 'briocheSerialize' method missing".into(),
        });
    } else {
        value_object
    };

    let recipe = deserialize_recipe(
        recipes,
        js_runtime,
        &value,
        module_namespace,
        cached_recipes,
    )
    .await?;
    cached_recipes.extend(equivalent_values.into_iter().map(|value| (value, recipe)));

    Ok(recipe)
}

#[expect(clippy::mutable_key_type)]
async fn deserialize_recipe(
    recipes: &mut crate::recipe::Recipes,
    js_runtime: &mut deno_core::JsRuntime,
    value: &deno_core::v8::Global<deno_core::v8::Object>,
    module_namespace: &deno_core::v8::Global<deno_core::v8::Value>,
    cached_recipes: &mut HashMap<deno_core::v8::Global<deno_core::v8::Value>, RecipeRef>,
) -> Result<RecipeRef, EvaluateError> {
    let ty = v8_get(js_runtime, value, "type")?;
    let ty = v8_cast::<_, deno_core::v8::String>(js_runtime, &ty)?;
    let ty = v8_string_to_string_lossy(js_runtime, &ty);
    let recipe = match &*ty {
        "create_file" => {
            let content = v8_get(js_runtime, value, "content")?;
            let content = v8_cast_string(js_runtime, &content)?;
            let content = tick_encoding::decode(content.as_bytes()).map_err(|error| {
                EvaluateError::InvalidRecipe {
                    reason: format!("invalid property 'content': {error}").into(),
                }
            })?;
            let content = bstr::BString::new(content.into_owned());

            let executable = v8_get(js_runtime, value, "executable")?;
            let executable = v8_cast_boolean(js_runtime, &executable)?;

            let resources = v8_get_nullish(js_runtime, value, "resources")?;
            let resources = if let Some(resources) = resources {
                Some(
                    Box::pin(eval_recipe(
                        recipes,
                        js_runtime,
                        &resources,
                        module_namespace,
                        cached_recipes,
                        false,
                    ))
                    .await?,
                )
            } else {
                None
            };

            Recipe::CreateFile {
                content,
                executable,
                resources,
            }
        }
        _ => {
            todo!();
        }
    };

    Ok(recipes.insert_recipe(Arc::new(recipe)))
}

fn v8_cast<T, U>(
    js_runtime: &mut deno_core::JsRuntime,
    value: &deno_core::v8::Global<T>,
) -> Result<deno_core::v8::Global<U>, EvaluateError>
where
    for<'s> deno_core::v8::Local<'s, U>: TryFrom<deno_core::v8::Local<'s, T>>,
    for<'s> <deno_core::v8::Local<'s, U> as TryFrom<deno_core::v8::Local<'s, T>>>::Error:
        Into<EvaluateError>,
{
    deno_core::scope!(js_scope, js_runtime);
    deno_core::v8::tc_scope!(let js_scope, js_scope);

    let value = deno_core::v8::Local::new(js_scope, value);
    let value = deno_core::v8::Local::try_from(value).map_err(Into::into)?;
    Ok(deno_core::v8::Global::new(js_scope, value))
}

fn v8_cast_string(
    js_runtime: &mut deno_core::JsRuntime,
    value: &deno_core::v8::Global<deno_core::v8::Value>,
) -> Result<String, EvaluateError> {
    deno_core::scope!(js_scope, js_runtime);
    deno_core::v8::tc_scope!(let js_scope, js_scope);

    let value = deno_core::v8::Local::new(js_scope, value);
    let value = deno_core::v8::Local::<deno_core::v8::String>::try_from(value)?;
    Ok(value.to_rust_string_lossy(js_scope))
}

fn v8_cast_boolean(
    js_runtime: &mut deno_core::JsRuntime,
    value: &deno_core::v8::Global<deno_core::v8::Value>,
) -> Result<bool, EvaluateError> {
    deno_core::scope!(js_scope, js_runtime);
    deno_core::v8::tc_scope!(let js_scope, js_scope);

    let value = deno_core::v8::Local::new(js_scope, value);
    let value = deno_core::v8::Local::<deno_core::v8::Boolean>::try_from(value)?;
    Ok(value.boolean_value(js_scope))
}

fn v8_call<'a>(
    js_runtime: &mut deno_core::JsRuntime,
    function: deno_core::v8::Global<deno_core::v8::Function>,
    this: Option<&deno_core::v8::Global<deno_core::v8::Value>>,
    args: impl IntoIterator<Item = &'a deno_core::v8::Global<deno_core::v8::Value>>,
) -> Result<deno_core::v8::Global<deno_core::v8::Value>, EvaluateError> {
    deno_core::scope!(js_scope, js_runtime);
    deno_core::v8::tc_scope!(let js_scope, js_scope);

    let function = deno_core::v8::Local::new(js_scope, function);
    let this = this.map_or_else(
        || deno_core::v8::undefined(js_scope).into(),
        |this| deno_core::v8::Local::new(js_scope, this),
    );
    let args = args
        .into_iter()
        .map(|arg| deno_core::v8::Local::new(js_scope, arg))
        .collect::<Vec<_>>();

    let result = function.call(js_scope, this, &args);
    let Some(result) = result else {
        if let Some(exception) = js_scope.exception() {
            return Err(deno_core::error::JsError::from_v8_exception(js_scope, exception).into());
        }
        return Err(EvaluateError::UnknownEvalError {
            reason: "function call failed without an exception".into(),
        });
    };

    Ok(deno_core::v8::Global::new(js_scope, result))
}

fn v8_string_inner<'s>(
    js_scope: &deno_core::v8::PinnedRef<'s, deno_core::v8::HandleScope<'_>>,
    s: &str,
) -> Result<deno_core::v8::Local<'s, deno_core::v8::String>, EvaluateError> {
    deno_core::v8::String::new(js_scope, s)
        .ok_or_else(|| EvaluateError::InvalidJsString(s.to_string()))
}

fn v8_string_to_string_lossy(
    js_runtime: &mut deno_core::JsRuntime,
    s: &deno_core::v8::Global<deno_core::v8::String>,
) -> String {
    deno_core::scope!(js_scope, js_runtime);
    deno_core::v8::tc_scope!(let js_scope, js_scope);

    let s = deno_core::v8::Local::new(js_scope, s);
    s.to_rust_string_lossy(js_scope)
}

fn v8_try_get_inner<'s>(
    js_scope: &deno_core::v8::PinnedRef<'s, deno_core::v8::HandleScope<'_>>,
    object: &deno_core::v8::Object,
    key: &str,
) -> Result<Option<deno_core::v8::Local<'s, deno_core::v8::Value>>, EvaluateError> {
    let key = v8_string_inner(js_scope, key)?;
    let key = key.into();

    let has_key = matches!(object.has(js_scope, key), Some(true));
    if !has_key {
        return Ok(None);
    }

    let value = object.get(js_scope, key);
    Ok(value)
}

fn v8_get_nullish(
    js_runtime: &mut deno_core::JsRuntime,
    object: &deno_core::v8::Global<deno_core::v8::Object>,
    key: &str,
) -> Result<Option<deno_core::v8::Global<deno_core::v8::Value>>, EvaluateError> {
    deno_core::scope!(js_scope, js_runtime);
    deno_core::v8::tc_scope!(let js_scope, js_scope);

    let object = deno_core::v8::Local::new(js_scope, object);
    let value = v8_try_get_inner(js_scope, &object, key)?;
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    Ok(Some(deno_core::v8::Global::new(js_scope, value)))
}

fn v8_get(
    js_runtime: &mut deno_core::JsRuntime,
    object: &deno_core::v8::Global<deno_core::v8::Object>,
    key: &str,
) -> Result<deno_core::v8::Global<deno_core::v8::Value>, EvaluateError> {
    deno_core::scope!(js_scope, js_runtime);
    deno_core::v8::tc_scope!(let js_scope, js_scope);

    let object = deno_core::v8::Local::new(js_scope, object);
    let value = v8_try_get_inner(js_scope, &object, key)?;
    let Some(value) = value else {
        return Err(EvaluateError::InvalidRecipe {
            reason: format!("object does not have property '{key}'").into(),
        });
    };
    Ok(deno_core::v8::Global::new(js_scope, value))
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
    InvalidJsString(String),

    #[error("module '{module_path}' does not have an export named '{export}'")]
    NoExport {
        module_ref: ModuleRef,
        module_path: AbsolutePath,
        export: String,
    },

    #[error("invalid recipe: {reason}")]
    InvalidRecipe { reason: Cow<'static, str> },

    #[error("unknown error while evaluating JS: {reason}")]
    UnknownEvalError { reason: Cow<'static, str> },

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

    #[error(transparent)]
    JsDataError(#[from] deno_core::v8::DataError),
}

impl From<std::convert::Infallible> for EvaluateError {
    fn from(value: std::convert::Infallible) -> Self {
        match value {}
    }
}
