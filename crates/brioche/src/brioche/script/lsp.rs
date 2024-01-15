use std::rc::Rc;

use anyhow::Context as _;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::request::GotoTypeDefinitionResponse;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::brioche::script::compiler_host::{brioche_compiler_host, BriocheCompilerHost};
use crate::brioche::script::format::format_code;
use crate::brioche::Brioche;

use super::specifier::BriocheModuleSpecifier;

pub struct BriocheLspServer {
    compiler_host: BriocheCompilerHost,
    client: Client,
    js_lsp: JsLspTask,
}

impl BriocheLspServer {
    pub async fn new(
        local_pool: &tokio_util::task::LocalPoolHandle,
        brioche: Brioche,
        client: Client,
    ) -> anyhow::Result<Self> {
        let compiler_host = BriocheCompilerHost::new(brioche.clone());
        let js_lsp = js_lsp_task(local_pool, compiler_host.clone());

        Ok(Self {
            compiler_host,
            client,
            js_lsp,
        })
    }

    async fn diagnostics(
        &self,
        text_document: TextDocumentIdentifier,
    ) -> anyhow::Result<Vec<Diagnostic>> {
        let diagnostics: Vec<Diagnostic> = self
            .js_lsp
            .send(JsLspMessage::Diagnostic(DocumentDiagnosticParams {
                identifier: None,
                previous_result_id: None,
                partial_result_params: Default::default(),
                text_document,
                work_done_progress_params: Default::default(),
            }))
            .await?;
        Ok(diagnostics)
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for BriocheLspServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        tracing::info!("initializing LSP");

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                completion_provider: Some(CompletionOptions::default()),
                definition_provider: Some(OneOf::Left(true)),
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions::default(),
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                references_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        tracing::info!("server initialized");
    }

    async fn shutdown(&self) -> Result<()> {
        tracing::info!("server shutting down");
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        tracing::info!(uri = %params.text_document.uri, "did open");

        let specifier = lsp_uri_to_module_specifier(&params.text_document.uri);
        match specifier {
            Ok(specifier) => {
                let result = self.compiler_host.load_document(&specifier).await;
                match result {
                    Ok(()) => {}
                    Err(error) => {
                        tracing::warn!("failed to load document {specifier}: {error:#}");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    "failed to parse URI {}: {error:#}",
                    params.text_document.uri
                );
            }
        }

        let diagnostics = self
            .diagnostics(TextDocumentIdentifier {
                uri: params.text_document.uri.clone(),
            })
            .await;
        match diagnostics {
            Ok(diagnostics) => {
                self.client
                    .publish_diagnostics(params.text_document.uri, diagnostics, None)
                    .await;
            }
            Err(error) => {
                tracing::error!("failed to get diagnostics: {error:#}");
            }
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        tracing::info!(uri = %params.text_document.uri, "did open");
        let diagnostics = self
            .diagnostics(TextDocumentIdentifier {
                uri: params.text_document.uri.clone(),
            })
            .await;
        match diagnostics {
            Ok(diagnostics) => {
                self.client
                    .publish_diagnostics(params.text_document.uri, diagnostics, None)
                    .await;
            }
            Err(error) => {
                tracing::error!("failed to get diagnostics: {error:#}");
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        tracing::info!(uri = %params.text_document.uri, "did update");
        if let Some(change) = params.content_changes.first() {
            let result = self
                .compiler_host
                .update_document(&params.text_document.uri, &change.text)
                .await;
            if result.is_err() {
                tracing::error!("failed to update document");
                return;
            }
        }

        let diagnostics = self
            .diagnostics(TextDocumentIdentifier {
                uri: params.text_document.uri.clone(),
            })
            .await;
        match diagnostics {
            Ok(diagnostics) => {
                self.client
                    .publish_diagnostics(params.text_document.uri, diagnostics, None)
                    .await;
            }
            Err(error) => {
                tracing::error!("failed to get diagnostics: {error:#}");
            }
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        tracing::info!("completion");
        let response = self
            .js_lsp
            .send::<Vec<CompletionItem>>(JsLspMessage::Completion(params.clone()))
            .await;
        let completions = match response {
            Ok(completions) => Some(CompletionResponse::Array(completions)),
            Err(error) => {
                tracing::error!(error = %error, "failed to get completion");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        Ok(completions)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        tracing::info!("goto definition");
        let response = self
            .js_lsp
            .send::<Option<Location>>(JsLspMessage::GotoDefintion(
                params.text_document_position_params.clone(),
            ))
            .await;
        let location = match response {
            Ok(location) => location.map(GotoDefinitionResponse::Scalar),
            Err(error) => {
                tracing::error!(error = %error, "failed to get goto definition");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        Ok(location)
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        tracing::info!("hover");
        let response = self
            .js_lsp
            .send::<Option<Hover>>(JsLspMessage::Hover(params.clone()))
            .await;
        let hover = match response {
            Ok(hover) => hover,
            Err(error) => {
                tracing::error!(error = %error, "failed to get hover");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        Ok(hover)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        tracing::info!("references");
        let response = self
            .js_lsp
            .send::<Option<Vec<Location>>>(JsLspMessage::References(params.clone()))
            .await;
        let references = match response {
            Ok(references) => references,
            Err(error) => {
                tracing::error!(error = %error, "failed to get hover");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        Ok(references)
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let response = self
            .js_lsp
            .send::<Option<Vec<DocumentHighlight>>>(JsLspMessage::DocumentHighlight(params.clone()))
            .await;
        let highlights = match response {
            Ok(highlights) => highlights,
            Err(error) => {
                tracing::error!(error = %error, "failed to get document highlight");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        Ok(highlights)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let response = self
            .js_lsp
            .send::<Option<PrepareRenameResponse>>(JsLspMessage::PrepareRename(params.clone()))
            .await;
        let rename = match response {
            Ok(rename) => rename,
            Err(error) => {
                tracing::error!(error = %error, "failed to prepare rename");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        Ok(rename)
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let prepare_rename = self
            .prepare_rename(params.text_document_position.clone())
            .await?;
        if prepare_rename.is_none() {
            return Err(tower_lsp::jsonrpc::Error::invalid_params("invalid rename"));
        }

        let response = self
            .js_lsp
            .send::<Option<WorkspaceEdit>>(JsLspMessage::Rename(params.clone()))
            .await;
        let workspace_edit = match response {
            Ok(workspace_edit) => workspace_edit,
            Err(error) => {
                tracing::error!(error = %error, "failed to rename");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };
        tracing::info!("rename: {:#?}", workspace_edit);
        Ok(workspace_edit)
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let specifier = lsp_uri_to_module_specifier(&params.text_document.uri);
        let specifier = match specifier {
            Ok(specifier) => specifier,
            Err(error) => {
                tracing::error!(error = %error, "failed to parse URI");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };

        let contents = self
            .compiler_host
            .read_loaded_document(specifier, |doc| doc.contents.clone());
        let contents = match contents {
            Ok(Some(contents)) => contents,
            Ok(None) => {
                tracing::error!("failed to find document");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
            Err(error) => {
                tracing::error!(error = %error, "failed to read document");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };

        let full_range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: contents
                    .lines()
                    .count()
                    .try_into()
                    .expect("could not convert lines"),
                character: 0,
            },
        };
        let formatted = format_code(&contents);
        let formatted = match formatted {
            Ok(formatted) => formatted,
            Err(error) => {
                tracing::error!(error = %error, "failed to format document");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };

        Ok(Some(vec![TextEdit {
            range: full_range,
            new_text: formatted,
        }]))
    }

    // async fn diagnostic(
    //     &self,
    //     params: DocumentDiagnosticParams,
    // ) -> Result<DocumentDiagnosticReportResult> {
    //     self.client
    //         .log_message(MessageType::INFO, "diagnostic")
    //         .await;
    //     let response = self
    //         .js_lsp
    //         .send::<Vec<Diagnostic>>(JsLspMessage::Diagnostic(params.clone()))
    //         .await;
    //     let diagnostics = match response {
    //         Ok(diagnostics) => diagnostics,
    //         Err(error) => {
    //             tracing::error!(error = %error, "failed to get diagnostics");
    //             return Err(tower_lsp::jsonrpc::Error::internal_error());
    //         }
    //     };

    //     tracing::info!(?diagnostics, "got diagnostics");

    //     Ok(DocumentDiagnosticReportResult::Report(
    //         DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
    //             full_document_diagnostic_report: FullDocumentDiagnosticReport {
    //                 items: diagnostics,
    //                 ..Default::default()
    //             },
    //             ..Default::default()
    //         }),
    //     ))
    // }
}

fn lsp_uri_to_module_specifier(uri: &url::Url) -> anyhow::Result<BriocheModuleSpecifier> {
    if uri.scheme() == "file" {
        let uri_string = uri.to_string();
        if uri_string.ends_with(".bri.ts") {
            let brioche_uri_string = uri_string
                .strip_suffix(".ts")
                .context("failed to truncate URI")?;
            let specifier = brioche_uri_string.parse()?;
            return Ok(specifier);
        }
    }

    let specifier = BriocheModuleSpecifier::try_from(uri)?;
    Ok(specifier)
}

const TIMEOUT_DURATION: std::time::Duration = std::time::Duration::from_secs(10);

struct JsLspTask {
    tx: tokio::sync::mpsc::Sender<(
        JsLspMessage,
        tokio::sync::oneshot::Sender<serde_json::Value>,
    )>,
}

impl JsLspTask {
    async fn send<T>(&self, message: JsLspMessage) -> anyhow::Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        self.tx.send((message, response_tx)).await?;

        // Await the response with a timeout
        let response = tokio::time::timeout(TIMEOUT_DURATION, response_rx)
            .await
            .map_err(|_| anyhow::anyhow!("timeout waiting for response from JS LSP"))??;

        let response = serde_json::from_value(response)?;

        Ok(response)
    }
}

fn js_lsp_task(
    local_pool: &tokio_util::task::LocalPoolHandle,
    compiler_host: BriocheCompilerHost,
) -> JsLspTask {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(
        JsLspMessage,
        tokio::sync::oneshot::Sender<serde_json::Value>,
    )>(1);

    let js_lsp_task = local_pool.spawn_pinned(|| async move {
        let module_loader = super::BriocheModuleLoader::new(&compiler_host.brioche);

        tracing::info!("building JS LSP");

        let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            module_loader: Some(Rc::new(module_loader.clone())),
            source_map_getter: Some(Box::new(module_loader.clone())),
            extensions: vec![
                brioche_compiler_host::init_ops(compiler_host),
                super::js::brioche_js::init_ops(),
            ],
            ..Default::default()
        });

        let main_module: deno_core::ModuleSpecifier = "briocheruntime:///dist/index.js".parse()?;

        tracing::info!(%main_module, "evaluating module");

        tracing::info!("loading module");

        let module_id = js_runtime.load_main_module(&main_module, None).await?;
        let result = js_runtime.mod_evaluate(module_id);
        js_runtime.run_event_loop(false).await?;
        result.await??;

        let module_namespace = js_runtime.get_module_namespace(module_id)?;

        tracing::info!("calling JS");

        let js_lsp = {
            let mut js_scope = js_runtime.handle_scope();
            let module_namespace = deno_core::v8::Local::new(&mut js_scope, module_namespace);

            let export_key_name = "buildLsp";
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

            tracing::info!(%main_module, ?export_key_name, "running function");

            let js_lsp = export
                .call(&mut js_scope, module_namespace.into(), &[])
                .context("failed to build LSP")?;
            let js_lsp: deno_core::v8::Local<deno_core::v8::Object> =
                js_lsp.try_into().context("expected LSP to be an object")?;
            deno_core::v8::Global::new(&mut js_scope, js_lsp)
        };

        tracing::info!("built JS LSP");

        while let Some((message, response_tx)) = rx.recv().await {
            tracing::info!(?message, "got message");
            let response = match message {
                JsLspMessage::Completion(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "completion", &params)
                }
                JsLspMessage::Diagnostic(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "diagnostic", &params)
                }
                JsLspMessage::GotoDefintion(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "gotoDefinition", &params)
                }
                JsLspMessage::Hover(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "hover", &params)
                }
                JsLspMessage::DocumentHighlight(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "documentHighlight", &params)
                }
                JsLspMessage::References(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "references", &params)
                }
                JsLspMessage::PrepareRename(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "prepareRename", &params)
                }
                JsLspMessage::Rename(params) => {
                    call_method_1(&mut js_runtime, &js_lsp, "rename", &params)
                }
            };

            match response {
                Ok(response) => {
                    let _ = response_tx.send(response);
                }
                Err(error) => {
                    tracing::error!("failed to call method: {error:#}");
                    return Err(error);
                }
            };
        }

        anyhow::Ok(())
    });

    tokio::task::spawn(async move {
        if let Err(error) = js_lsp_task.await {
            tracing::error!("error in JS LSP task: {error}");
        }
    });

    JsLspTask { tx }
}

fn call_method(
    runtime: &mut deno_core::JsRuntime,
    this: &deno_core::v8::Global<deno_core::v8::Object>,
    method_name: &str,
    args: &[deno_core::v8::Global<deno_core::v8::Value>],
) -> anyhow::Result<serde_json::Value> {
    let mut js_scope = runtime.handle_scope();
    let value = deno_core::v8::Local::new(&mut js_scope, this);
    let method_key = deno_core::v8::String::new(&mut js_scope, method_name)
        .context("failed to create V8 string")?;
    let method = value
        .get(&mut js_scope, method_key.into())
        .context("failed to get property")?;
    let method: deno_core::v8::Local<deno_core::v8::Function> = method
        .try_into()
        .context("expected property to be a function")?;

    let args = args
        .iter()
        .map(|arg| deno_core::v8::Local::new(&mut js_scope, arg))
        .collect::<Vec<_>>();

    let mut js_scope = deno_core::v8::TryCatch::new(&mut js_scope);
    let result = method.call(&mut js_scope, value.into(), &args);
    let result = match result {
        Some(result) => result,
        None => {
            if let Some(exception) = js_scope.exception() {
                return Err(deno_core::error::JsError::from_v8_exception(
                    &mut js_scope,
                    exception,
                ))
                .with_context(|| format!("error when calling {method_name:?}"));
            } else {
                anyhow::bail!("unknown error when calling {method_name:?}");
            }
        }
    };

    let result = serde_v8::from_v8(&mut js_scope, result)?;

    Ok(result)
}

fn call_method_1(
    js_runtime: &mut deno_core::JsRuntime,
    this: &deno_core::v8::Global<deno_core::v8::Object>,
    method_name: &str,
    arg_1: &impl serde::Serialize,
) -> anyhow::Result<serde_json::Value> {
    let arg_1 = {
        let mut js_scope = js_runtime.handle_scope();
        let value = serde_v8::to_v8(&mut js_scope, arg_1)?;
        deno_core::v8::Global::new(&mut js_scope, value)
    };
    call_method(js_runtime, this, method_name, &[arg_1])
}

#[derive(Debug, Clone)]
enum JsLspMessage {
    Completion(CompletionParams),
    Diagnostic(DocumentDiagnosticParams),
    GotoDefintion(TextDocumentPositionParams),
    Hover(HoverParams),
    DocumentHighlight(DocumentHighlightParams),
    References(ReferenceParams),
    PrepareRename(TextDocumentPositionParams),
    Rename(RenameParams),
}
