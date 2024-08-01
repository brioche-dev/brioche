use std::rc::Rc;

use anyhow::Context as _;

use crate::{
    bake::BakeScope,
    project::{ProjectHash, Projects},
    recipe::{Recipe, WithMeta},
    Brioche,
};

use super::BriocheModuleLoader;

#[tracing::instrument(skip(brioche, projects, project_hash), fields(%project_hash), err)]
pub async fn evaluate(
    brioche: &Brioche,
    projects: &Projects,
    project_hash: ProjectHash,
    export: &str,
) -> anyhow::Result<WithMeta<Recipe>> {
    let module_loader_task = super::ModuleLoaderTask::new(brioche.clone(), projects.clone());
    let runtime_task = super::RuntimeTask::new(brioche.clone(), projects.clone());
    let main_module = projects.project_root_module_specifier(project_hash)?;

    let result = evaluate_with_deno(
        project_hash,
        export.to_string(),
        main_module,
        module_loader_task,
        runtime_task,
    )
    .await?;
    Ok(result)
}

async fn evaluate_with_deno(
    project_hash: ProjectHash,
    export: String,
    main_module: super::specifier::BriocheModuleSpecifier,
    module_loader_task: super::ModuleLoaderTask,
    runtime_task: super::RuntimeTask,
) -> anyhow::Result<WithMeta<Recipe>> {
    let (result_tx, result_rx) =
        tokio::sync::oneshot::channel::<anyhow::Result<WithMeta<Recipe>>>();

    let bake_scope = BakeScope::Project {
        project_hash,
        export: export.clone(),
    };

    std::thread::spawn(move || {
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

        let result = runtime.block_on(async move {
            let module_loader = BriocheModuleLoader::new(module_loader_task);
            let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
                module_loader: Some(Rc::new(module_loader)),
                extensions: vec![
                    super::brioche_rt::init_ops(runtime_task, bake_scope),
                    super::js::brioche_js::init_ops(),
                ],
                ..Default::default()
            });

            let main_module: deno_core::ModuleSpecifier = main_module.into();

            tracing::debug!(%main_module, "evaluating module");

            let module_id = js_runtime.load_main_es_module(&main_module).await?;
            let result = js_runtime.mod_evaluate(module_id);
            js_runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default())
                .await?;
            result.await?;

            let module_namespace = js_runtime.get_module_namespace(module_id)?;

            let result = {
                let mut js_scope = js_runtime.handle_scope();
                let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);

                let module_namespace = deno_core::v8::Local::new(&mut js_scope, module_namespace);

                let export_key = deno_core::v8::String::new(&mut js_scope, &export)
                    .context("failed to create V8 string")?;
                let export_value = module_namespace
                    .get(&mut js_scope, export_key.into())
                    .with_context(|| format!("expected module to have an export named {export}"))?;
                let export_value: deno_core::v8::Local<deno_core::v8::Function> =
                    export_value
                        .try_into()
                        .with_context(|| format!("expected export named {export} to be a function"))?;

                tracing::debug!(%main_module, %export, "running exported function");

                let result = export_value.call(&mut js_scope, module_namespace.into(), &[]);

                tracing::error!("result: {:?}", result);

                let result = match result {
                    Some(result) => result,
                    None => {
                        if let Some(exception) = js_scope.exception() {
                            return Err(anyhow::anyhow!(
                                deno_core::error::JsError::from_v8_exception(&mut js_scope, exception)
                            ))
                            .with_context(|| format!("error when calling {export}"));
                        } else {
                            anyhow::bail!("unknown error when calling {export}");
                        }
                    }
                };
                deno_core::v8::Global::new(&mut js_scope, result)
            };

            tracing::error!("resolving...");

            let resolved_result = js_runtime.resolve(result).await?;

            tracing::error!("serializing...");

            let serialized_result = {
                let mut js_scope = js_runtime.handle_scope();
                let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);

                let resolved_result = deno_core::v8::Local::new(&mut js_scope, resolved_result);
                let resolved_result: deno_core::v8::Local<deno_core::v8::Object> = resolved_result
                    .try_into()
                    .context("expected result to be an object")?;

                let serialize_key = deno_core::v8::String::new(&mut js_scope, "briocheSerialize")
                    .context("failed to create V8 string")?;
                let result_serialize = resolved_result
                    .get(&mut js_scope, serialize_key.into())
                    .context("expected value to have a `briocheSerialize` function")?;
                let result_serialize: deno_core::v8::Local<deno_core::v8::Function> = result_serialize
                    .try_into()
                    .context("expected `briocheSerialize` to be a function")?;

                let serialized_result = result_serialize.call(&mut js_scope, resolved_result.into(), &[]);
                let serialized_result = match serialized_result {
                    Some(serialized_result) => serialized_result,
                    None => {
                        if let Some(exception) = js_scope.exception() {
                            return Err(anyhow::anyhow!(
                                deno_core::error::JsError::from_v8_exception(&mut js_scope, exception)
                            ))
                            .with_context(|| format!("error when serializing result from {export}"));
                        } else {
                            anyhow::bail!("unknown error when serializing result from {export}");
                        }
                    }
                };
                deno_core::v8::Global::new(&mut js_scope, serialized_result)
            };

            let serialized_resolved_result = js_runtime.resolve(serialized_result).await?;

            let mut js_scope = js_runtime.handle_scope();

            let serialized_resolved_result =
                deno_core::v8::Local::new(&mut js_scope, serialized_resolved_result);

            let recipe: WithMeta<Recipe> = serde_v8::from_v8(&mut js_scope, serialized_resolved_result)
                .with_context(|| {
                    format!("invalid recipe returned when serializing result from {export}")
                })?;

            tracing::debug!(%main_module, recipe_hash = %recipe.hash(), "finished evaluating module");

            Ok(recipe)
        });

        tracing::error!("finished");

        let _ = result_tx.send(result);
    });

    let result = result_rx.await??;
    Ok(result)
}
