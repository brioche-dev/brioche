use std::rc::Rc;

use anyhow::Context as _;

use crate::brioche::{
    artifact::{LazyArtifact, WithMeta},
    script::specifier::BriocheModuleSpecifier,
    Brioche,
};

use super::{BriocheModuleLoader, Project};

#[tracing::instrument(skip(brioche, project), err)]
pub async fn evaluate(
    brioche: &Brioche,
    project: &Project,
    export: &str,
) -> anyhow::Result<WithMeta<LazyArtifact>> {
    let module_loader = BriocheModuleLoader::new(brioche);
    let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
        module_loader: Some(Rc::new(module_loader.clone())),
        source_map_getter: Some(Box::new(module_loader.clone())),
        extensions: vec![
            super::brioche_rt::init_ops(brioche.clone()),
            super::js::brioche_js::init_ops(),
        ],
        ..Default::default()
    });

    js_runtime.execute_script_static(
        "[brioche_init]",
        r#"
            // Use Deno's stack trace routine, which resolves sourcemaps
            Error.prepareStackTrace = Deno.core.prepareStackTrace;
        "#,
    )?;

    let main_module = BriocheModuleSpecifier::File {
        path: project.local_path.join("project.bri"),
    };
    let main_module: deno_core::ModuleSpecifier = main_module.into();

    tracing::info!(path = %project.local_path.display(), %main_module, "evaluating module");

    let module_id = js_runtime.load_main_module(&main_module, None).await?;
    let result = js_runtime.mod_evaluate(module_id);
    js_runtime.run_event_loop(false).await?;
    result.await??;

    let module_namespace = js_runtime.get_module_namespace(module_id)?;

    let result = {
        let mut js_scope = js_runtime.handle_scope();
        let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);

        let module_namespace = deno_core::v8::Local::new(&mut js_scope, module_namespace);

        let export_key = deno_core::v8::String::new(&mut js_scope, export)
            .context("failed to create V8 string")?;
        let export_value = module_namespace
            .get(&mut js_scope, export_key.into())
            .with_context(|| format!("expected module to have an export named {export}"))?;
        let export_value: deno_core::v8::Local<deno_core::v8::Function> =
            export_value
                .try_into()
                .with_context(|| format!("expected export named {export} to be a function"))?;

        tracing::info!(path = %project.local_path.display(), %main_module, %export, "running exported function");

        let result = export_value.call(&mut js_scope, module_namespace.into(), &[]);
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

    let resolved_result = js_runtime.resolve_value(result).await?;

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

    let serialized_resolved_result = js_runtime.resolve_value(serialized_result).await?;

    let mut js_scope = js_runtime.handle_scope();

    let serialized_resolved_result =
        deno_core::v8::Local::new(&mut js_scope, serialized_resolved_result);

    let artifact: WithMeta<LazyArtifact> =
        serde_v8::from_v8(&mut js_scope, serialized_resolved_result).with_context(|| {
            format!("invalid artifact returned when serializing result from {export}")
        })?;

    tracing::debug!(path = %project.local_path.display(), %main_module, artifact_hash = %artifact.hash(), "finished evaluating module");

    Ok(artifact)
}
