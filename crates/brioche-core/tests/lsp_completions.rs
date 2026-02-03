use brioche_core::cache::CacheClient;
use futures::TryStreamExt as _;
use tower_lsp::lsp_types::{
    self, Position, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    notification, request,
};
use url::Url;

use crate::lsp_types::PartialResultParams;
use crate::lsp_types::WorkDoneProgressParams;

// Timeout used to wait for messages back from the LSP. This is mainly used
// as a sanity check to prevent a test from hanging forever
const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

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
    });

    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(4, 11),
            ),
            context: None,
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
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
    });

    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(2, 26),
            ),
            context: None,
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
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

#[tokio::test]
async fn test_lsp_completions_from_local_registry_import() -> anyhow::Result<()> {
    let (_brioche, context, lsp) = brioche_test_support::brioche_lsp_test().await;

    let (foo_hash, _) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export function fooFunction() {
                        return "hello world!";
                    }
                "#,
            )
            .await
            .unwrap();
        })
        .await;

    let project_dir = context.mkdir("myproject").await;

    // Use a lockfile where 'foo' points to a project we already have locally
    let lockfile = brioche_core::project::Lockfile {
        dependencies: std::iter::once(("foo".to_string(), foo_hash)).collect(),
        ..Default::default()
    };
    context
        .write_lockfile(project_dir.join("brioche.lock"), &lockfile)
        .await;

    let project_root_contents = indoc::indoc! {r#"
        import * as foo from "foo";

        foo.fooFunctio // <-- completion here
    "#};
    let project_root = context
        .write_file(project_dir.join("project.bri"), project_root_contents)
        .await;

    lsp.notify::<notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(
            Url::from_file_path(&project_root).unwrap(),
            "brioche".to_string(),
            1,
            project_root_contents.to_string(),
        ),
    });

    // Get completions. This should be able to complete the function from
    // 'foo' since it's already saved locally
    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(2, 14),
            ),
            context: None,
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
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
            label: "fooFunction".to_string(),
            ..Default::default()
        },
    );

    Ok(())
}

#[tokio::test]
async fn test_lsp_completions_from_remote_registry_import() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (_brioche, mut context, lsp) = brioche_test_support::brioche_lsp_test_with({
        let cache = cache.clone();
        move |builder| {
            builder.cache_client(CacheClient {
                store: Some(cache.clone()),
                ..Default::default()
            })
        }
    })
    .await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export function fooFunction() {
                        return "hello world!";
                    }
                "#,
            )
            .await
            .unwrap();
        })
        .await;

    let project_dir = context.mkdir("myproject").await;

    let lockfile = brioche_core::project::Lockfile {
        dependencies: std::iter::once(("foo".to_string(), foo_hash)).collect(),
        ..Default::default()
    };
    context
        .write_lockfile(project_dir.join("brioche.lock"), &lockfile)
        .await;

    let project_root_contents = indoc::indoc! {r#"
        import * as foo from "foo";

        foo.fooFunctio // <-- completion here
    "#};
    let project_root = context
        .write_file(project_dir.join("project.bri"), project_root_contents)
        .await;

    // Subscribe to diagnostics messages from the LSP server
    let diagnostics_stream = lsp.subscribe_to_notifications::<notification::PublishDiagnostics>();
    let mut diagnostics_stream = std::pin::pin!(diagnostics_stream);

    lsp.notify::<notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(
            Url::from_file_path(&project_root).unwrap(),
            "brioche".to_string(),
            1,
            project_root_contents.to_string(),
        ),
    });

    // Consume a diagnostics notification triggered by opening the document
    tokio::time::timeout(WAIT_TIMEOUT, diagnostics_stream.try_next())
        .await
        .unwrap()
        .unwrap();

    // Trigger completions before 'foo' has been pulled from the cache. This
    // should not find any completions, since the LSP should not pull from
    // the cache just by triggering completions
    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(2, 14),
            ),
            context: None,
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
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
    assert!(
        completion_items.is_empty(),
        "expected no completions: {completion_items:#?}"
    );

    Ok(())
}

#[tokio::test]
async fn test_lsp_completions_from_workspace_import() -> anyhow::Result<()> {
    let (_brioche, context, lsp) = brioche_test_support::brioche_lsp_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./foo".parse().unwrap()],
            },
        )
        .await;

    context
        .write_file(
            "myworkspace/foo/project.bri",
            indoc::indoc! {r#"
                export function workspaceFunction() {
                    return "hello world!";
                }
            "#},
        )
        .await;

    let project_root_contents = indoc::indoc! {r#"
        import * as foo from "foo";

        foo.workspaceFunctio // <-- completion here
    "#};
    let project_root = context
        .write_file("myworkspace/myproject/project.bri", project_root_contents)
        .await;

    lsp.notify::<notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(
            Url::from_file_path(&project_root).unwrap(),
            "brioche".to_string(),
            1,
            project_root_contents.to_string(),
        ),
    });

    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(2, 20),
            ),
            context: None,
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
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
            label: "workspaceFunction".to_string(),
            ..Default::default()
        },
    );

    Ok(())
}

#[tokio::test]
async fn test_lsp_completions_in_unsaved_edited_file() -> anyhow::Result<()> {
    let (_brioche, context, lsp) = brioche_test_support::brioche_lsp_test().await;

    let project_root = context
        .write_file(
            "myproject/project.bri",
            indoc::indoc! {r"
                // File on disk is effectively empty
            "},
        )
        .await;

    // The existing file exists but doesn't match the content provided by
    // the LSP client. This happens e.g. when re-opening a VS Code window
    // that has unsaved file changes. In this case, we should use the client's
    // view of the file's contents instead of what's actually on disk
    lsp.notify::<notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(
            Url::from_file_path(&project_root).unwrap(),
            "brioche".to_string(),
            1,
            indoc::indoc! {r#"
                function exampleFunction(): string {
                    return "hello world!";
                }

                exampleFunc // <-- completion here
            "#}
            .to_string(),
        ),
    });

    let completion_response = lsp
        .request::<request::Completion>(lsp_types::CompletionParams {
            text_document_position: TextDocumentPositionParams::new(
                TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
                Position::new(4, 11),
            ),
            context: None,
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
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
