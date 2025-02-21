use tower_lsp::lsp_types::{
    self, notification, request, Position, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams,
};
use url::Url;

#[tokio::test]
async fn test_lsp_completions_simple() -> anyhow::Result<()> {
    let (_brioche, context, lsp) = brioche_test_support::brioche_lsp_test().await;

    let project_root_contents = indoc::indoc! {r#"
        function exampleFunction(): string {
            return "hello world!";
        }

        exampleFunc // <-- completion here
    "#};
    let project_root = context
        .write_file("myproject/project.bri", project_root_contents)
        .await;

    lsp.notify::<notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(
            Url::from_file_path(&project_root).unwrap(),
            "brioche".to_string(),
            1,
            project_root_contents.to_string(),
        ),
    })
    .await;

    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(4, 11),
            ),
            context: None,
            partial_result_params: Default::default(),
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("no completion response returned");
    let completion_items = match completion_response {
        lsp_types::CompletionResponse::Array(completion_items) => completion_items,
        lsp_types::CompletionResponse::List(completion_list) => {
            assert!(
                !completion_list.is_incomplete,
                "expected complete completion list"
            );
            completion_list.items
        }
    };
    assert!(!completion_items.is_empty(), "no completions returned");

    let top_completion_item = &completion_items[0];
    assert_eq!(
        top_completion_item,
        &lsp_types::CompletionItem {
            label: "exampleFunction".to_string(),
            ..Default::default()
        },
    );

    Ok(())
}

#[tokio::test]
async fn test_lsp_completions_from_import() -> anyhow::Result<()> {
    let (_brioche, context, lsp) = brioche_test_support::brioche_lsp_test().await;

    context
        .write_file(
            "myproject/other.bri",
            indoc::indoc! {r#"
                export function exampleFunctionFromOther() {
                    return "hello world!";
                }
            "#},
        )
        .await;

    let project_root_contents = indoc::indoc! {r#"
        import * as other from "./other.bri";

        other.exampleFunctionFromO // <-- completion here
    "#};
    let project_root = context
        .write_file("myproject/project.bri", project_root_contents)
        .await;

    lsp.notify::<notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(
            Url::from_file_path(&project_root).unwrap(),
            "brioche".to_string(),
            1,
            project_root_contents.to_string(),
        ),
    })
    .await;

    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(2, 26),
            ),
            context: None,
            partial_result_params: Default::default(),
            work_done_progress_params: Default::default(),
        })
        .await
        .expect("no completion response returned");
    let completion_items = match completion_response {
        lsp_types::CompletionResponse::Array(completion_items) => completion_items,
        lsp_types::CompletionResponse::List(completion_list) => {
            assert!(
                !completion_list.is_incomplete,
                "expected complete completion list"
            );
            completion_list.items
        }
    };
    assert!(!completion_items.is_empty(), "no completions returned");

    let top_completion_item = &completion_items[0];
    assert_eq!(
        top_completion_item,
        &lsp_types::CompletionItem {
            label: "exampleFunctionFromOther".to_string(),
            ..Default::default()
        },
    );

    Ok(())
}
