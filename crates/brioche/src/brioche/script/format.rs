use std::rc::Rc;

use anyhow::Context as _;

use crate::brioche::{project::Project, Brioche};

#[tracing::instrument(skip(brioche, project), err)]
pub async fn format(brioche: &Brioche, project: &Project) -> anyhow::Result<()> {
    let mut formatter = Formatter::new(brioche).await?;

    for path in project.local_module_paths() {
        let contents = tokio::fs::read_to_string(path).await?;

        let formatted_contents = formatter.format_code(&contents).await?;

        tokio::fs::write(path, &formatted_contents).await?;
    }

    Ok(())
}

struct Formatter {
    js_runtime: deno_core::JsRuntime,
    module_id: usize,
}

impl Formatter {
    #[tracing::instrument(skip_all)]
    async fn new(brioche: &Brioche) -> anyhow::Result<Self> {
        let module_loader = super::BriocheModuleLoader::new(brioche);
        let compiler_host = super::compiler_host::BriocheCompilerHost::new(brioche.clone());
        let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            module_loader: Some(Rc::new(module_loader.clone())),
            source_map_getter: Some(Box::new(module_loader.clone())),
            extensions: vec![
                super::compiler_host::brioche_compiler_host::init_ops(compiler_host),
                super::js::brioche_js::init_ops(),
            ],
            ..Default::default()
        });

        let main_module: deno_core::ModuleSpecifier = "briocheruntime:///dist/index.js".parse()?;

        tracing::debug!(%main_module, "evaluating module");

        let module_id = js_runtime.load_main_module(&main_module, None).await?;
        let result = js_runtime.mod_evaluate(module_id);
        js_runtime.run_event_loop(false).await?;
        result.await??;

        Ok(Self {
            js_runtime,
            module_id,
        })
    }

    #[tracing::instrument(skip_all)]
    async fn format_code(&mut self, source: &str) -> anyhow::Result<String> {
        let module_namespace = self.js_runtime.get_module_namespace(self.module_id)?;

        let result = {
            let mut js_scope = self.js_runtime.handle_scope();
            let module_namespace = deno_core::v8::Local::new(&mut js_scope, module_namespace);

            let export_key_name = "format";
            let export_key = deno_core::v8::String::new(&mut js_scope, export_key_name)
                .context("failed to create V8 string")?;
            let export = module_namespace
                .get(&mut js_scope, export_key.into())
                .with_context(|| {
                    format!("expected module to have an export named {export_key_name:?}")
                })?;
            let export: deno_core::v8::Local<deno_core::v8::Function> = export
                .try_into()
                .with_context(|| format!("expected export {export_key_name:?} to be a function"))?;

            tracing::debug!(?export_key_name, "running function");

            let v8_source = serde_v8::to_v8(&mut js_scope, source)?;

            let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);

            let result = export.call(&mut js_scope, module_namespace.into(), &[v8_source]);
            let result = match result {
                Some(result) => result,
                None => {
                    let error_message = js_scope
                        .exception()
                        .map(|exception| {
                            anyhow::anyhow!(deno_core::error::JsError::from_v8_exception(
                                &mut js_scope,
                                exception
                            ))
                        })
                        .unwrap_or_else(|| anyhow::anyhow!("unknown error when calling function"));
                    return Err(error_message)
                        .with_context(|| format!("error when calling {export_key_name:?}"));
                }
            };
            deno_core::v8::Global::new(&mut js_scope, result)
        };

        let resolved_result = self.js_runtime.resolve_value(result).await?;

        let formatted_source: String = {
            let mut js_scope = self.js_runtime.handle_scope();
            let resolved_result = deno_core::v8::Local::new(&mut js_scope, resolved_result);

            serde_v8::from_v8(&mut js_scope, resolved_result)?
        };

        Ok(formatted_source)
    }
}

#[tracing::instrument(skip(brioche, source), err)]
async fn format_code(brioche: &Brioche, source: &str) -> anyhow::Result<String> {
    let module_loader = super::BriocheModuleLoader::new(brioche);
    let compiler_host = super::compiler_host::BriocheCompilerHost::new(brioche.clone());
    let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
        module_loader: Some(Rc::new(module_loader.clone())),
        source_map_getter: Some(Box::new(module_loader.clone())),
        extensions: vec![
            super::compiler_host::brioche_compiler_host::init_ops(compiler_host),
            super::js::brioche_js::init_ops(),
        ],
        ..Default::default()
    });

    let main_module: deno_core::ModuleSpecifier = "briocheruntime:///dist/index.js".parse()?;

    tracing::debug!(%main_module, "evaluating module");

    let module_id = js_runtime.load_main_module(&main_module, None).await?;
    let result = js_runtime.mod_evaluate(module_id);
    js_runtime.run_event_loop(false).await?;
    result.await??;

    let module_namespace = js_runtime.get_module_namespace(module_id)?;

    let result = {
        let mut js_scope = js_runtime.handle_scope();
        let module_namespace = deno_core::v8::Local::new(&mut js_scope, module_namespace);

        let export_key_name = "format";
        let export_key = deno_core::v8::String::new(&mut js_scope, export_key_name)
            .context("failed to create V8 string")?;
        let export = module_namespace
            .get(&mut js_scope, export_key.into())
            .with_context(|| {
                format!("expected module to have an export named {export_key_name:?}")
            })?;
        let export: deno_core::v8::Local<deno_core::v8::Function> = export
            .try_into()
            .with_context(|| format!("expected export {export_key_name:?} to be a function"))?;

        tracing::debug!(%main_module, ?export_key_name, "running function");

        let v8_source = serde_v8::to_v8(&mut js_scope, source)?;

        let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);

        let result = export.call(&mut js_scope, module_namespace.into(), &[v8_source]);
        let result = match result {
            Some(result) => result,
            None => {
                let error_message = js_scope
                    .exception()
                    .map(|exception| {
                        anyhow::anyhow!(deno_core::error::JsError::from_v8_exception(
                            &mut js_scope,
                            exception
                        ))
                    })
                    .unwrap_or_else(|| anyhow::anyhow!("unknown error when calling function"));
                return Err(error_message)
                    .with_context(|| format!("error when calling {export_key_name:?}"));
            }
        };
        deno_core::v8::Global::new(&mut js_scope, result)
    };

    let resolved_result = js_runtime.resolve_value(result).await?;

    let formatted_source: String = {
        let mut js_scope = js_runtime.handle_scope();
        let resolved_result = deno_core::v8::Local::new(&mut js_scope, resolved_result);

        serde_v8::from_v8(&mut js_scope, resolved_result)?
    };

    tracing::debug!(path = %main_module, "finished evaluating module");

    Ok(formatted_source)
}
