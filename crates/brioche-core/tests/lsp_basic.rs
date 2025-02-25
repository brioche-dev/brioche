use assert_matches::assert_matches;
use tower_lsp::lsp_types::{self, TextDocumentSyncKind};

#[tokio::test]
async fn test_lsp_basic_initialize() -> anyhow::Result<()> {
    let (_brioche, _context, lsp) = brioche_test_support::brioche_lsp_test().await;

    // Validate that the LSP server returned some of the capabilities we expect

    assert_matches!(
        lsp.initialize_result.capabilities.completion_provider,
        Some(_)
    );
    assert_matches!(
        lsp.initialize_result.capabilities.definition_provider,
        Some(_)
    );
    assert_matches!(
        lsp.initialize_result.capabilities.diagnostic_provider,
        Some(_)
    );
    assert_matches!(lsp.initialize_result.capabilities.hover_provider, Some(_));
    assert_eq!(
        lsp.initialize_result.capabilities.text_document_sync,
        Some(lsp_types::TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::FULL
        ))
    );
    assert_matches!(
        lsp.initialize_result.capabilities.references_provider,
        Some(_)
    );
    assert_matches!(
        lsp.initialize_result
            .capabilities
            .document_highlight_provider,
        Some(_)
    );
    assert_matches!(lsp.initialize_result.capabilities.rename_provider, Some(_));
    assert_matches!(
        lsp.initialize_result
            .capabilities
            .document_formatting_provider,
        Some(_)
    );

    Ok(())
}
