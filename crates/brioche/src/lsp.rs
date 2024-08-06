use clap::Parser;

#[derive(Debug, Parser)]
pub struct LspArgs {
    /// Use stdio for LSP transport
    #[arg(long)]
    stdio: bool,
}

pub async fn lsp(_args: LspArgs) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let local_pool = tokio_util::task::LocalPoolHandle::new(5);

    let (service, socket) = tower_lsp::LspService::new(move |client| {
        let local_pool = &local_pool;
        futures::executor::block_on(async move {
            let (reporter, _guard) = brioche_core::reporter::start_lsp_reporter(client.clone());
            let brioche = brioche_core::BriocheBuilder::new(reporter)
                .registry_client(brioche_core::registry::RegistryClient::disabled())
                .vfs(brioche_core::vfs::Vfs::mutable())
                .build()
                .await?;
            let projects = brioche_core::project::Projects::default();
            let lsp_server =
                brioche_core::script::lsp::BriocheLspServer::new(brioche, projects, client).await?;
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
