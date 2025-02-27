use std::sync::Arc;

use clap::Parser;
use futures::FutureExt as _;

#[derive(Debug, Parser)]
pub struct LspArgs {
    /// Use stdio for LSP transport
    #[arg(long)]
    stdio: bool,
}

pub async fn lsp(_args: LspArgs) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = tower_lsp::LspService::new(move |client| {
        futures::executor::block_on(async move {
            let (reporter, _guard) = brioche_core::reporter::start_lsp_reporter(client.clone());
            let brioche = brioche_core::BriocheBuilder::new(reporter)
                .registry_client(brioche_core::registry::RegistryClient::disabled())
                .cache_client(brioche_core::cache::CacheClient::default())
                .vfs(brioche_core::vfs::Vfs::mutable())
                .build()
                .await?;
            let projects = brioche_core::project::Projects::default();

            let (null_reporter, _null_guard) = brioche_core::reporter::start_null_reporter();
            let remote_brioche_builder = move || {
                let null_reporter = null_reporter.clone();
                let remote_brioche_fut = async move {
                    let remote_brioche_builder =
                        brioche_core::BriocheBuilder::new(null_reporter.clone());
                    anyhow::Ok(remote_brioche_builder)
                };
                remote_brioche_fut.boxed()
            };

            let lsp_server = brioche_core::script::lsp::BriocheLspServer::new(
                brioche,
                projects,
                client,
                Arc::new(remote_brioche_builder),
            )?;
            anyhow::Ok(lsp_server)
        })
        .expect("failed to build LSP")
    });

    // Note: For now, we always use stdio for the LSP
    tower_lsp::Server::new(stdin, stdout, socket)
        .serve(service)
        .await;

    Ok(())
}
