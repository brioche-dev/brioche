use std::path::PathBuf;

use assert_matches::assert_matches;
use brioche::brioche::project::resolve_project;
use brioche::brioche::script::check::{CheckResult, DiagnosticLevel};

mod brioche_test;

async fn write_project(context: &brioche_test::TestContext, name: &str, script: &str) -> PathBuf {
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
    let (brioche, context) = brioche_test::brioche_test().await;

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
    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project).await?;

    assert_matches!(worst_level(&result), None);

    Ok(())
}

#[tokio::test]
async fn test_check_basic_invalid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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
    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project).await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Error));

    Ok(())
}

#[tokio::test]
async fn test_check_import_valid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    context
        .write_file(
            "brioche-repo/foo/project.bri",
            r#"
                export const project = {};
                export const foo: number = 123;
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project).await?;

    assert_matches!(worst_level(&result), None);

    Ok(())
}

#[tokio::test]
async fn test_check_import_invalid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    context
        .write_file(
            "brioche-repo/foo/project.bri",
            r#"
                export const project = {};
                export const foo: string = 123;
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project).await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Error));

    Ok(())
}

#[tokio::test]
async fn test_check_import_nonexistent() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project).await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Error));

    Ok(())
}

#[tokio::test]
async fn test_check_invalid_unused_var() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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
    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project).await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Warning));

    Ok(())
}

#[tokio::test]
async fn test_check_invalid_missing_await() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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
    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project).await?;

    assert_matches!(worst_level(&result), Some(DiagnosticLevel::Warning));

    Ok(())
}
