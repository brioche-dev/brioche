use brioche_core::cache::CacheClient;
use futures::TryStreamExt as _;
use tower_lsp::lsp_types::{
    self, PartialResultParams, Position, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, WorkDoneProgressParams, notification, request,
};
use url::Url;

// Timeout used to wait for messages back from the LSP. This is mainly used
// as a sanity check to prevent a test from hanging forever
const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[tokio::test]
async fn test_lsp_on_save_respects_existing_lock() -> anyhow::Result<()> {
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
async fn test_lsp_on_save_fetches_locked_dependency() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context, lsp) = brioche_test_support::brioche_lsp_test_with({
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
    // the cache until the user saves
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

    // Trigger a save to make the LSP fetch 'foo' from the cache
    lsp.notify::<notification::DidSaveTextDocument>(lsp_types::DidSaveTextDocumentParams {
        text_document: TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
        text: None,
    });

    // Wait for the server to publish diagnostics after saving. This is
    // the most reliable indicator currently that the server finished
    // fetching 'foo' after saving (but relies on the implementation detail
    // that the LSP server publishes diagnostics at this point)
    tokio::time::timeout(WAIT_TIMEOUT, diagnostics_stream.try_next())
        .await
        .unwrap()
        .unwrap();

    // Ensure 'foo' was downloaded
    let foo_path = brioche.data_dir.join("projects").join(foo_hash.to_string());
    assert!(
        tokio::fs::try_exists(&foo_path).await.unwrap(),
        "expected remote project 'foo' to be saved after saving in LSP"
    );

    // Validate the hash of 'foo'
    {
        let (brioche, _context) = brioche_test_support::brioche_test().await;
        let (_projects, project_hash) = brioche_test_support::load_project(&brioche, &foo_path)
            .await
            .expect("failed to load remote project 'foo' after saving project in LSP");
        assert_eq!(project_hash, foo_hash);
    }

    // Trigger completions again. This should include completions from 'foo'
    // now that 'foo' has been downloaded from the cache
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
async fn test_lsp_on_save_fetches_dependency_and_updates_lockfile() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context, lsp) = brioche_test_support::brioche_lsp_test_with({
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
    context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;

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
    // the cache / registry until the user saves
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

    // Trigger a save to make the LSP fetch 'foo' from the cache
    lsp.notify::<notification::DidSaveTextDocument>(lsp_types::DidSaveTextDocumentParams {
        text_document: TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
        text: None,
    });

    // Wait for the server to publish diagnostics after saving. This is
    // the most reliable indicator currently that the server finished
    // fetching 'foo' after saving (but relies on the implementation detail
    // that the LSP server publishes diagnostics at this point)
    tokio::time::timeout(WAIT_TIMEOUT, diagnostics_stream.try_next())
        .await
        .unwrap()
        .unwrap();

    // Ensure 'foo' was downloaded
    let foo_path = brioche.data_dir.join("projects").join(foo_hash.to_string());
    assert!(
        tokio::fs::try_exists(&foo_path).await.unwrap(),
        "expected remote project 'foo' to be saved after saving in LSP"
    );

    // Validate the hash of 'foo'
    {
        let (brioche, _context) = brioche_test_support::brioche_test().await;
        let (_projects, project_hash) = brioche_test_support::load_project(&brioche, &foo_path)
            .await
            .expect("failed to load remote project 'foo' after saving project in LSP");
        assert_eq!(project_hash, foo_hash);
    }

    // Validate that the lockfile was created properly
    let project_lockfile_path = project_dir.join("brioche.lock");
    assert!(tokio::fs::try_exists(&project_lockfile_path).await.unwrap());
    let project_lockfile_contents = tokio::fs::read_to_string(&project_lockfile_path).await?;
    let project_lockfile: brioche_core::project::Lockfile =
        serde_json::from_str(&project_lockfile_contents)?;
    assert_eq!(
        project_lockfile,
        brioche_core::project::Lockfile {
            dependencies: std::iter::once(("foo".into(), foo_hash)).collect(),
            ..Default::default()
        },
    );

    // Trigger completions again. This should include completions from 'foo'
    // now that 'foo' has been downloaded from the cache
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
async fn test_lsp_on_save_only_adds_new_dependencies_in_lockfile() -> anyhow::Result<()> {
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
    context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let bar_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export function barFunction() {
                        return "hello world!";
                    }
                "#,
            )
            .await
            .unwrap();
        })
        .await;

    let project_dir = context.mkdir("myproject").await;

    let project_root_contents = indoc::indoc! {r#"
        import * as foo from "foo";

        Brioche.download("https://example.org/");
        Brioche.gitRef({
            repository: "https://example.org/repo.git",
            ref: "main",
        });
    "#};
    let project_root = context
        .write_file(project_dir.join("project.bri"), project_root_contents)
        .await;

    // Initial lockfile contains out-of-date dependencies, downloads, and
    // git refs
    let project_initial_lockfile = brioche_core::project::Lockfile {
        dependencies: std::iter::once(("bar".into(), bar_hash)).collect(),
        downloads: std::iter::once((
            "https://example.com/".parse().unwrap(),
            brioche_core::Hasher::new_sha256().finish().unwrap(),
        ))
        .collect(),
        git_refs: std::iter::once((
            "https://example.com/repo.git".parse().unwrap(),
            std::iter::once((
                "main".into(),
                "bb43d030babf4df990d7294f10f74ef7e23f7c21".into(),
            ))
            .collect(),
        ))
        .collect(),
    };
    context
        .write_lockfile(project_dir.join("brioche.lock"), &project_initial_lockfile)
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

    // Trigger a save, which should make the LSP partially update the lockfile
    lsp.notify::<notification::DidSaveTextDocument>(lsp_types::DidSaveTextDocumentParams {
        text_document: TextDocumentIdentifier::new(Url::from_file_path(&project_root).unwrap()),
        text: None,
    });

    // Wait for the server to publish diagnostics after saving. This is
    // the most reliable indicator currently that the LSP finished with saving
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        diagnostics_stream.try_next(),
    )
    .await
    .unwrap()
    .unwrap();

    let project_lockfile_path = project_dir.join("brioche.lock");
    assert!(tokio::fs::try_exists(&project_lockfile_path).await.unwrap());
    let project_lockfile_contents = tokio::fs::read_to_string(&project_lockfile_path).await?;
    let project_lockfile: brioche_core::project::Lockfile =
        serde_json::from_str(&project_lockfile_contents)?;

    // Validate that the lockfile only added 'foo' as a dependency. Statics
    // and unused dependencies should not be touched when saving in the LSP
    let expected_updated_lockfile = brioche_core::project::Lockfile {
        dependencies: [("foo".into(), foo_hash), ("bar".into(), bar_hash)]
            .into_iter()
            .collect(),
        ..project_initial_lockfile
    };
    assert_eq!(project_lockfile, expected_updated_lockfile);

    Ok(())
}
