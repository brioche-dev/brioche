use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Context as _;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::request::GotoTypeDefinitionResponse;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::project::{ProjectLocking, ProjectValidation, Projects};
use crate::script::compiler_host::{brioche_compiler_host, BriocheCompilerHost};
use crate::script::format::format_code;
use crate::{Brioche, BriocheBuilder};

use super::bridge::RuntimeBridge;
use super::specifier::BriocheModuleSpecifier;

/// The maximum time we spend resolving projects when regenerating a
/// lockfile in the Language Server
const LOCKFILE_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub type BuildBriocheFn = dyn Fn() -> futures::future::BoxFuture<'static, anyhow::Result<BriocheBuilder>>
    + Send
    + Sync
    + 'static;

pub struct BriocheLspServer {
    bridge: RuntimeBridge,
    compiler_host: BriocheCompilerHost,
    client: Client,
    js_lsp: JsLspTask,
    remote_brioche_builder: Arc<BuildBriocheFn>,
}

impl BriocheLspServer {
    pub async fn new(
        brioche: Brioche,
        projects: Projects,
        client: Client,
        remote_brioche_builder: Arc<BuildBriocheFn>,
    ) -> anyhow::Result<Self> {
        let bridge = RuntimeBridge::new(brioche.clone(), projects.clone());

        let compiler_host = BriocheCompilerHost::new(bridge.clone()).await;
        let js_lsp = js_lsp_task(compiler_host.clone(), bridge.clone());

        Ok(Self {
            bridge,
            compiler_host,
            client,
            js_lsp,
            remote_brioche_builder,
        })
    }

    async fn load_document(&self, uri: &url::Url) -> anyhow::Result<()> {
        let specifier = lsp_uri_to_module_specifier(uri)?;
        self.compiler_host
            .load_documents(vec![specifier.clone()])
            .await?;
        Ok(())
    }

    async fn load_document_if_not_loaded(&self, uri: &url::Url) -> anyhow::Result<()> {
        let specifier = lsp_uri_to_module_specifier(uri)?;

        if !self.compiler_host.is_document_loaded(&specifier).await? {
            self.compiler_host
                .load_documents(vec![specifier.clone()])
                .await?;
        }

        Ok(())
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
        let text_document_uri = &params.text_document.uri;

        tracing::info!(uri = %text_document_uri, "did open");

        let result = self
            .compiler_host
            .update_document(text_document_uri, &params.text_document.text)
            .await;
        if let Err(error) = result {
            tracing::warn!(
                "failed to update document {text_document_uri} while opening: {error:#}",
            );
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
        tracing::info!(uri = %params.text_document.uri, "did save");

        if let Ok(BriocheModuleSpecifier::File { path: module_path }) =
            (&params.text_document.uri).try_into()
        {
            let rt = tokio::runtime::Handle::current();
            let (lockfile_tx, lockfile_rx) = tokio::sync::oneshot::channel();

            std::thread::spawn({
                let remote_brioche_builder = self.remote_brioche_builder.clone();
                let bridge = self.bridge.clone();
                let module_path = module_path.clone();
                move || {
                    let local_set = tokio::task::LocalSet::new();

                    local_set.spawn_local(async move {
                        let result = try_update_lockfile_for_module(
                            remote_brioche_builder,
                            bridge,
                            module_path,
                            LOCKFILE_LOAD_TIMEOUT,
                        )
                        .await;
                        let _ = lockfile_tx.send(result).inspect_err(|err| {
                            tracing::warn!("failed to send lockfile update result: {err:?}");
                        });
                    });

                    rt.block_on(local_set);
                }
            });

            let result = lockfile_rx
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            match result {
                Ok(true) => {
                    tracing::info!("updated lockfile for {}", module_path.display());
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!("failed to update lockfiles: {error:#}");
                }
            }
        }

        // Try to reload the project for the current document so we can
        // resolve new dependencies
        let _ = self
            .compiler_host
            .reload_module_project(&params.text_document.uri)
            .await
            .inspect_err(|error| {
                tracing::warn!("failed to reload module project: {error:#}");
            });

        // Reload the current document. This ensures any new referenced
        // modules are loaded too
        let _ = self
            .load_document(&params.text_document.uri)
            .await
            .inspect_err(|error| {
                tracing::warn!("failed to load document after saving: {error:#}");
            });

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
        if let Some(change) = params.content_changes.first() {
            let result = self
                .compiler_host
                .update_document(&params.text_document.uri, &change.text)
                .await;
            if let Err(error) = result {
                tracing::error!("failed to update document: {error:#}");
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

        let text_document_uri = &params.text_document_position.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during completion request: {error:#}"
            );
        }

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

        let text_document_uri = &params.text_document_position_params.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during goto definition request: {error:#}"
            );
        }

        let response = self
            .js_lsp
            .send::<Option<Location>>(JsLspMessage::GotoDefinition(
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

        let text_document_uri = &params.text_document_position_params.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during hover request: {error:#}"
            );
        }

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

        let text_document_uri = &params.text_document_position.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during references request: {error:#}"
            );
        }

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
        let text_document_uri = &params.text_document_position_params.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during document highlight request: {error:#}"
            );
        }

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
        tracing::info!("prepare rename");

        let text_document_uri = &params.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during prepare rename request: {error:#}"
            );
        }

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
        tracing::info!("rename");

        let text_document_uri = &params.text_document_position.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during rename request: {error:#}"
            );
        }

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
        let text_document_uri = &params.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during formatting request: {error:#}"
            );
        }

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

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> Result<DocumentDiagnosticReportResult> {
        self.client
            .log_message(MessageType::INFO, "diagnostic")
            .await;

        let text_document_uri = &params.text_document.uri;
        let load_result = self.load_document_if_not_loaded(text_document_uri).await;
        if let Err(error) = load_result {
            tracing::warn!(
                "failed to load document {text_document_uri} during diagnostic request: {error:#}"
            );
        }

        let response = self
            .js_lsp
            .send::<Vec<Diagnostic>>(JsLspMessage::Diagnostic(params.clone()))
            .await;
        let diagnostics = match response {
            Ok(diagnostics) => diagnostics,
            Err(error) => {
                tracing::error!(error = %error, "failed to get diagnostics");
                return Err(tower_lsp::jsonrpc::Error::internal_error());
            }
        };

        tracing::info!(?diagnostics, "got diagnostics");

        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    items: diagnostics,
                    ..Default::default()
                },
                ..Default::default()
            }),
        ))
    }
}

async fn try_update_lockfile_for_module(
    build_remote_brioche: Arc<BuildBriocheFn>,
    bridge: RuntimeBridge,
    module_path: PathBuf,
    load_timeout: std::time::Duration,
) -> anyhow::Result<bool> {
    let project_path = bridge.project_root_for_module_path(module_path).await?;

    // Build a "remote" Brioche instance, and load the project into a blank
    // `Projects` instance to avoid conflicts with the `Projects` instance
    // used throughout the LSP (where files may be dirty in-memory
    // copies). This will let us update the lockfile starting from a blank
    // slate, and properly pull any new registry imports.
    let remote_brioche = (build_remote_brioche)().await?.build().await?;
    let projects = Projects::default();
    tokio::time::timeout(
        load_timeout,
        projects.load(
            &remote_brioche,
            &project_path,
            ProjectValidation::Minimal,
            ProjectLocking::Unlocked,
        ),
    )
    .await
    .context("timed out trying to load project")?
    .context("failed to load project")?;

    let updated = projects
        .commit_dirty_lockfile_for_project_path(&project_path)
        .await
        .context("failed to commit updated lockfile")?;

    Ok(updated)
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

        let response = serde_json::from_value(response.clone()).inspect_err(|_| {
            tracing::warn!(
                "failed to deserialize response: {:?}",
                serde_json::to_string(&response)
            )
        })?;

        Ok(response)
    }
}

fn js_lsp_task(compiler_host: BriocheCompilerHost, bridge: RuntimeBridge) -> JsLspTask {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(
        JsLspMessage,
        tokio::sync::oneshot::Sender<serde_json::Value>,
    )>(1);

    std::thread::spawn(move || {
        let module_loader = super::BriocheModuleLoader::new(bridge);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!("failed to create runtime: {error:#}");
                return;
            }
        };

        let result = runtime.block_on(async move {
            tracing::info!("building JS LSP");

            let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
                module_loader: Some(Rc::new(module_loader)),
                extensions: vec![
                    brioche_compiler_host::init_ops(compiler_host),
                    super::js::brioche_js::init_ops(),
                ],
                ..Default::default()
            });

            let main_module: deno_core::ModuleSpecifier =
                "briocheruntime:///dist/index.js".parse()?;

            tracing::info!(%main_module, "evaluating module");

            tracing::info!("loading module");

            let module_id = js_runtime.load_main_es_module(&main_module).await?;
            let result = js_runtime.mod_evaluate(module_id);
            js_runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default())
                .await?;
            result.await?;

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
                let export: deno_core::v8::Local<deno_core::v8::Function> =
                    export.try_into().with_context(|| {
                        format!("expected export {export_key_name:?} to be a function")
                    })?;

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
                    JsLspMessage::GotoDefinition(params) => {
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

        if let Err(error) = result {
            tracing::error!("failed to run runtime: {error:#}");
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
    GotoDefinition(TextDocumentPositionParams),
    Hover(HoverParams),
    DocumentHighlight(DocumentHighlightParams),
    References(ReferenceParams),
    PrepareRename(TextDocumentPositionParams),
    Rename(RenameParams),
}
