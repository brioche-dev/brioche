use std::rc::Rc;

use anyhow::Context as _;

use crate::{
    Brioche,
    bake::BakeScope,
    project::{ProjectHash, Projects},
    recipe::{Recipe, WithMeta},
};

use super::{BriocheModuleLoader, bridge::RuntimeBridge};

#[tracing::instrument(skip(brioche, projects, project_hash), fields(%project_hash), err)]
pub async fn evaluate(
    brioche: &Brioche,
    js_platform: super::JsPlatform,
    projects: &Projects,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<WithMeta<Recipe>> {
    let bridge = RuntimeBridge::new(brioche.clone(), projects.clone());
    let main_module = projects.project_root_module_specifier(project_hash)?;

    let result = evaluate_with_deno(
        js_platform,
        project_hash,
        export.to_string(),
        main_module,
        bridge,
    )
    .await?;
    Ok(result)
}

async fn evaluate_with_deno(
    _js_platform: super::JsPlatform,
    project_hash: ProjectHash,
    export: String,
    main_module: super::specifier::BriocheModuleSpecifier,
    bridge: RuntimeBridge,
) -> anyhow::Result<WithMeta<Recipe>> {
    // Create a channel to get the result from the other Tokio runtime
    let (result_tx, result_rx) =
        tokio::sync::oneshot::channel::<anyhow::Result<WithMeta<Recipe>>>();

    let bake_scope = BakeScope::Project {
        project_hash,
        export: export.clone(),
    };

    // Spawn a new thread for the new Tokio runtime
    std::thread::spawn(move || {
        // Create a new Tokio runtime
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = result_tx.send(Err(error.into()));
                return;
            }
        };

        // Spawn the main JS task on the new runtime. See this issue for
        // more context on why this is required:
        // https://github.com/brioche-dev/brioche/pull/105#issuecomment-2241289605
        let result = runtime.block_on(async move {
            // Create the runtime
            let module_loader = BriocheModuleLoader::new(bridge.clone());
            let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
                module_loader: Some(Rc::new(module_loader)),
                extensions: vec![
                    super::brioche_rt::init(bridge, bake_scope),
                    super::js::brioche_js::init(),
                ],
                ..Default::default()
            });

            let main_module: deno_core::ModuleSpecifier = main_module.into();

            tracing::debug!(%main_module, "evaluating module");

            // Load and evaluate the main module
            let module_id = js_runtime.load_main_es_module(&main_module).await?;
            let result = js_runtime.mod_evaluate(module_id);
            js_runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default())
                .await?;
            result.await?;

            let module_namespace = js_runtime.get_module_namespace(module_id)?;

            // Get the provided export from the module, and call it if
            // it's a function
            let result = {
                deno_core::scope!(js_scope, js_runtime);
                deno_core::v8::tc_scope!(let js_scope, js_scope);

                let module_namespace = deno_core::v8::Local::new(js_scope, module_namespace);

                // Get the export by name
                let export_key = deno_core::v8::String::new(js_scope, &export)
                    .context("failed to create V8 string")?;
                let export_value = module_namespace
                    .get(js_scope, export_key.into())
                    .with_context(|| format!("expected module to have an export named {export}"))?;

                let result = if export_value.is_function() {
                    // If the export is a function, call it

                    let export_value: deno_core::v8::Local<deno_core::v8::Function> =
                        export_value
                            .try_into()
                            .with_context(|| format!("expected export named {export} to be a function"))?;

                    tracing::debug!(%main_module, %export, "running exported function");

                    let result = export_value.call(js_scope, module_namespace.into(), &[]);
                    let Some(result) = result else {
                        if let Some(exception) = js_scope.exception() {
                            return Err(anyhow::anyhow!(
                                deno_core::error::JsError::from_v8_exception(js_scope, exception)
                            ))
                            .with_context(|| format!("error when calling {export}"));
                        }
                        anyhow::bail!("unknown error when calling {export}");
                    };

                    result
                } else {
                    // Otherwise, return it as-is

                    export_value
                };

                deno_core::v8::Global::new(js_scope, result)
            };

            // Resolve the export if it's a promise
            let resolved_result_fut = js_runtime.resolve(result);
            let resolved_result = js_runtime.with_event_loop_promise(resolved_result_fut, deno_core::PollEventLoopOptions::default()).await?;

            // Call the `briocheSerialize` function on the result
            let serialized_result = {
                deno_core::scope!(js_scope, js_runtime);
                deno_core::v8::tc_scope!(let js_scope, js_scope);

                let resolved_result = deno_core::v8::Local::new(js_scope, resolved_result);
                let resolved_result: deno_core::v8::Local<deno_core::v8::Object> = resolved_result
                    .try_into()
                    .context("expected result to be an object")?;

                let serialize_key = deno_core::v8::String::new(js_scope, "briocheSerialize")
                    .context("failed to create V8 string")?;
                let result_serialize = resolved_result
                    .get(js_scope, serialize_key.into())
                    .context("expected value to have a `briocheSerialize` function")?;
                let result_serialize: deno_core::v8::Local<deno_core::v8::Function> = result_serialize
                    .try_into()
                    .context("expected `briocheSerialize` to be a function")?;

                let serialized_result = result_serialize.call(js_scope, resolved_result.into(), &[]);
                let Some(serialized_result) = serialized_result else {
                    if let Some(exception) = js_scope.exception() {
                        return Err(anyhow::anyhow!(
                            deno_core::error::JsError::from_v8_exception(js_scope, exception)
                        ))
                        .with_context(|| format!("error when serializing result from {export}"));
                    }
                    anyhow::bail!("unknown error when serializing result from {export}");
                };

                deno_core::v8::Global::new(js_scope, serialized_result)
            };

            // Resolve the result of `briocheSerialize` if it's a promise
            let serialized_resolved_result_fut = js_runtime.resolve(serialized_result);
            let serialized_resolved_result = js_runtime.with_event_loop_promise(serialized_resolved_result_fut, deno_core::PollEventLoopOptions::default()).await?;

            deno_core::scope!(js_scope, js_runtime);

            let serialized_resolved_result =
                deno_core::v8::Local::new(js_scope, serialized_resolved_result);

            // Deserialize the result as a recipe
            let recipe: WithMeta<Recipe> = serde_v8::from_v8(js_scope, serialized_resolved_result)
                .with_context(|| {
                    format!("invalid recipe returned when serializing result from {export}")
                })?;

            tracing::debug!(%main_module, recipe_hash = %recipe.hash(), "finished evaluating module");

            Ok(recipe)
        });

        let _ = result_tx.send(result);
    });

    let result = result_rx.await??;
    Ok(result)
}
