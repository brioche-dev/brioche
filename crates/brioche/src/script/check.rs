use std::rc::Rc;

use anyhow::Context as _;

use crate::{
    project::{ProjectHash, Projects},
    vfs::Vfs,
    Brioche,
};

use super::specifier::BriocheModuleSpecifier;

#[tracing::instrument(skip(brioche, projects), err)]
pub async fn check(
    brioche: &Brioche,
    projects: &Projects,
    project_hash: ProjectHash,
) -> anyhow::Result<CheckResult> {
    let specifier = projects.project_root_module_specifier(project_hash)?;

    let module_loader = super::BriocheModuleLoader::new(brioche, projects);
    let compiler_host =
        super::compiler_host::BriocheCompilerHost::new(brioche.clone(), projects.clone()).await;
    compiler_host.load_document(&specifier).await?;

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

    tracing::info!(%specifier, %main_module, "evaluating module");

    let module_id = js_runtime.load_main_module(&main_module, None).await?;
    let result = js_runtime.mod_evaluate(module_id);
    js_runtime.run_event_loop(false).await?;
    result.await??;

    let module_namespace = js_runtime.get_module_namespace(module_id)?;

    let mut js_scope = js_runtime.handle_scope();
    let module_namespace = deno_core::v8::Local::new(&mut js_scope, module_namespace);

    let export_key_name = "check";
    let export_key = deno_core::v8::String::new(&mut js_scope, export_key_name)
        .context("failed to create V8 string")?;
    let export = module_namespace
        .get(&mut js_scope, export_key.into())
        .with_context(|| format!("expected module to have an export named {export_key_name:?}"))?;
    let export: deno_core::v8::Local<deno_core::v8::Function> = export
        .try_into()
        .with_context(|| format!("expected export {export_key_name:?} to be a function"))?;

    tracing::info!(%specifier, %main_module, ?export_key_name, "running function");

    let files = projects.project_module_specifiers(project_hash)?;
    let files = serde_v8::to_v8(&mut js_scope, &files)?;

    let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);

    let result = export.call(&mut js_scope, module_namespace.into(), &[files]);
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

    let diagnostics: Vec<Diagnostic> = serde_v8::from_v8(&mut js_scope, result)?;

    tracing::debug!(%specifier, %main_module, "finished evaluating module");

    Ok(CheckResult { diagnostics })
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
