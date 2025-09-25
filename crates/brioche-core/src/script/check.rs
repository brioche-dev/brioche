use std::{collections::HashSet, rc::Rc};

use anyhow::Context as _;

use crate::{
    Brioche,
    project::{ProjectHash, Projects},
    vfs::Vfs,
};

use super::{bridge::RuntimeBridge, specifier::BriocheModuleSpecifier};

#[tracing::instrument(skip(brioche, projects), err)]
pub async fn check(
    brioche: &Brioche,
    js_platform: super::JsPlatform,
    projects: &Projects,
    project_hashes: &HashSet<ProjectHash>,
) -> anyhow::Result<CheckResult> {
    let project_specifiers = projects.project_module_specifiers_for_projects(project_hashes)?;

    let runtime_bridge = RuntimeBridge::new(brioche.clone(), projects.clone());

    let result = check_with_deno(js_platform, project_specifiers, runtime_bridge).await?;
    Ok(result)
}

async fn check_with_deno(
    _js_platform: super::JsPlatform,
    project_specifiers: HashSet<super::specifier::BriocheModuleSpecifier>,
    bridge: RuntimeBridge,
) -> anyhow::Result<CheckResult> {
    // Create a channel to get the result from the other Tokio runtime
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();

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
            let module_loader = super::BriocheModuleLoader::new(bridge.clone());
            let compiler_host = super::compiler_host::BriocheCompilerHost::new(bridge);

            // Load all of the provided specifiers
            compiler_host
                .load_documents(project_specifiers.iter().cloned().collect())
                .await?;

            let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
                module_loader: Some(Rc::new(module_loader)),
                extensions: vec![
                    super::compiler_host::brioche_compiler_host::init(compiler_host),
                    super::js::brioche_js::init(),
                ],
                ..Default::default()
            });

            // Get the specifier for the built-in runtime module
            let main_module: deno_core::ModuleSpecifier =
                "briocheruntime:///dist/index.js".parse()?;

            tracing::debug!(%main_module, "evaluating module");

            // Load and evaluate the runtime module
            let module_id = js_runtime.load_main_es_module(&main_module).await?;
            let result = js_runtime.mod_evaluate(module_id);
            js_runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default())
                .await?;
            result.await?;

            let module_namespace = js_runtime.get_module_namespace(module_id)?;

            let mut js_scope = js_runtime.handle_scope();
            let module_namespace = deno_core::v8::Local::new(&mut js_scope, module_namespace);

            // Get the `check` function from the module
            let export_key_name = "check";
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

            // Call `check` with the project specifiers

            let files = serde_v8::to_v8(&mut js_scope, &project_specifiers)?;

            let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);

            let result = export.call(&mut js_scope, module_namespace.into(), &[files]);
            let Some(result) = result else {
                let error_message = js_scope.exception().map_or_else(
                    || anyhow::anyhow!("unknown error when calling function"),
                    |exception| {
                        anyhow::anyhow!(deno_core::error::JsError::from_v8_exception(
                            &mut js_scope,
                            exception
                        ))
                    },
                );
                return Err(error_message)
                    .with_context(|| format!("error when calling {export_key_name:?}"));
            };

            // Deserialize the result as an array of `Dignostic` values

            let diagnostics: Vec<Diagnostic> = serde_v8::from_v8(&mut js_scope, result)?;

            tracing::debug!(%main_module, "finished evaluating module");

            Ok(CheckResult { diagnostics })
        });

        let _ = result_tx.send(result);
    });

    let result = result_rx.await??;
    Ok(result)
}

pub struct CheckResult {
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckResult {
    fn diagnostics(&self, worst_level: DiagnosticLevel) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(move |diag| diag.message.level >= worst_level)
    }

    pub fn ensure_ok(&self, worst_level: DiagnosticLevel) -> Result<(), DiagnosticError> {
        let diagnostics: Vec<_> = self.diagnostics(worst_level).cloned().collect();
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(DiagnosticError { diagnostics })
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub specifier: Option<BriocheModuleSpecifier>,
    pub start: Option<u64>,
    pub length: Option<u64>,
    pub message: DiagnosticMessage,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Message,
    Suggestion,
    Warning,
    Error,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticMessage {
    pub level: DiagnosticLevel,
    text: String,
    nested: Vec<DiagnosticMessage>,
}

#[derive(Debug)]
pub struct DiagnosticError {
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticError {
    pub fn write(&self, vfs: &Vfs, out: &mut impl std::io::Write) -> anyhow::Result<()> {
        for (n, diagnostic) in self.diagnostics.iter().enumerate() {
            if n != 0 {
                writeln!(out)?;
            }

            let level = &diagnostic.message.level;

            let location = diagnostic.specifier.as_ref().zip(diagnostic.start.as_ref());
            if let Some((specifier, index)) = location {
                let contents = super::specifier::read_specifier_contents(vfs, specifier)?;
                let (line, col) = index_to_line_col(&contents, *index)?;
                writeln!(out, "[{level:?}] {specifier}:{line}:{col}")?;
            } else {
                writeln!(out, "[{level:?}]")?;
            }

            write_diagnostic(&diagnostic.message, out, 0)?;
        }

        Ok(())
    }
}

fn write_diagnostic(
    message: &DiagnosticMessage,
    out: &mut impl std::io::Write,
    indent: usize,
) -> std::io::Result<()> {
    let indentation = " ".repeat(indent * 2);
    writeln!(out, "{}{}", indentation, message.text)?;
    for nested in &message.nested {
        write_diagnostic(nested, out, indent + 1)?;
    }
    Ok(())
}

fn index_to_line_col(code: &[u8], index: u64) -> anyhow::Result<(u64, u64)> {
    let index: usize = index
        .try_into()
        .with_context(|| format!("index {index} too large"))?;

    let mut line = 1;
    let mut col = 1;
    let mut current_index = 0;
    let mut bytes = code.iter();
    while current_index < index {
        let Some(&byte) = bytes.next() else {
            anyhow::bail!("index {index} out of bounds");
        };

        if byte == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
        current_index += 1;
    }
    Ok((line, col))
}

#[cfg(test)]
mod tests {
    use super::index_to_line_col;

    use assert_matches::assert_matches;

    #[test]
    fn test_index_to_line_col() {
        let input = b"Hello\nworld";
        assert_eq!(index_to_line_col(input, 0).unwrap(), (1, 1));
        assert_eq!(index_to_line_col(input, 1).unwrap(), (1, 2));
        assert_eq!(index_to_line_col(input, 5).unwrap(), (1, 6));
        assert_eq!(index_to_line_col(input, 6).unwrap(), (2, 1));
        assert_eq!(index_to_line_col(input, 7).unwrap(), (2, 2));
        assert_matches!(index_to_line_col(input, 12), Err(_));
    }
}
