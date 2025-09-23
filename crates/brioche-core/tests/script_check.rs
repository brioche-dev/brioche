use std::{collections::HashSet, hash::RandomState, path::PathBuf};

use assert_matches::assert_matches;
use brioche_core::script::check::{CheckResult, DiagnosticLevel};

async fn write_project(
    context: &brioche_test_support::TestContext,
    name: &str,
    script: &str,
) -> PathBuf {
    let project_dir = context.mkdir(name).await;

    context.write_file("myproject/project.bri", script).await;

    project_dir
}

fn worst_level(result: &CheckResult) -> Option<DiagnosticLevel> {
    result
        .diagnostics
        .iter()
        .map(|diag| diag.message.level)
        .max()
}

#[tokio::test]
async fn test_check_basic_valid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        r#"
            export const project = {};
            export const foo: number = 123;
            export default () => {
                return {
                    briocheSerialize: () => {
                        return {
                            type: "directory",
                            entries: {},
                        }
                    },
                };
            };
        "#,
    )
    .await;
    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    let project_hashes: HashSet<_, RandomState> = HashSet::from_iter([project_hash]);

    let result = brioche_core::script::check::check(
        &brioche,
        brioche_core::script::initialize_js_platform(),
        &projects,
        &project_hashes,
    )
    .await?;

    assert_matches!(worst_level(&result), None);

    Ok(())
}

#[tokio::test]
async fn test_check_basic_invalid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        r#"
            export const project = {};
            export const foo: number = "123";
            export default () => {
                return {
                    briocheSerialize: () => {
                        return {
                            type: "directory",
                            entries: {},
                        }
                    },
                };
            };
        "#,
    )
    .await;
    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    let project_hashes: HashSet<_, RandomState> = HashSet::from_iter([project_hash]);

    let result = brioche_core::script::check::check(
        &brioche,
        brioche_core::script::initialize_js_platform(),
        &projects,
        &project_hashes,
    )
    .await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Error));

    Ok(())
}

#[tokio::test]
async fn test_check_import_valid() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        r#"
            import { foo } from "foo";
            export const project = {
                dependencies: {
                    foo: "*",
                },
            };
            function ignore(_value: unknown): void {}
            export default () => {
                ignore(foo);
                return {
                    briocheSerialize: () => {
                        return {
                            type: "directory",
                            entries: {},
                        }
                    },
                };
            };
        "#,
    )
    .await;

    let (foo_hash, _) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                r"
                    export const project = {};
                    export const foo: number = 123;
                ",
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    let project_hashes: HashSet<_, RandomState> = HashSet::from_iter([project_hash]);

    let result = brioche_core::script::check::check(
        &brioche,
        brioche_core::script::initialize_js_platform(),
        &projects,
        &project_hashes,
    )
    .await?;

    assert_matches!(worst_level(&result), None);

    Ok(())
}

#[tokio::test]
async fn test_check_import_nonexistent() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        r#"
            import { foo } from "foo";
            export const project = {};
            export default () => {
                return {
                    briocheSerialize: () => {
                        return {
                            type: "directory",
                            entries: {},
                        }
                    },
                };
            };
        "#,
    )
    .await;

    let (projects, project_hash) =
        brioche_test_support::load_project_no_validate(&brioche, &project_dir).await?;
    let project_hashes: HashSet<_, RandomState> = HashSet::from_iter([project_hash]);

    let result = brioche_core::script::check::check(
        &brioche,
        brioche_core::script::initialize_js_platform(),
        &projects,
        &project_hashes,
    )
    .await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Error));

    Ok(())
}

#[tokio::test]
async fn test_check_invalid_unused_var() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        r#"
            export const project = {};
            export default () => {
                const foo: number = 123;
                return {
                    briocheSerialize: () => {
                        return {
                            type: "directory",
                            entries: {},
                        }
                    },
                };
            };
        "#,
    )
    .await;
    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    let project_hashes: HashSet<_, RandomState> = HashSet::from_iter([project_hash]);

    let result = brioche_core::script::check::check(
        &brioche,
        brioche_core::script::initialize_js_platform(),
        &projects,
        &project_hashes,
    )
    .await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Warning));

    Ok(())
}

#[tokio::test]
async fn test_check_invalid_missing_await() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        r#"
            export const project = {};

            function foo(): Promise<void> {
                return new Promise((resolve) => {
                    resolve();
                });
            }

            export default () => {
                foo();
                return {
                    briocheSerialize: () => {
                        return {
                            type: "directory",
                            entries: {},
                        }
                    },
                };
            };
        "#,
    )
    .await;
    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;
    let project_hashes: HashSet<_, RandomState> = HashSet::from_iter([project_hash]);

    let result = brioche_core::script::check::check(
        &brioche,
        brioche_core::script::initialize_js_platform(),
        &projects,
        &project_hashes,
    )
    .await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Warning));

    Ok(())
}
