use std::path::PathBuf;

use assert_matches::assert_matches;
use brioche::brioche::project::resolve_project;
use brioche::brioche::script::check::DiagnosticLevel;

mod brioche_test;

async fn write_project(context: &brioche_test::TestContext, name: &str, script: &str) -> PathBuf {
    let project_dir = context.mkdir(name).await;

    context.write_file("myproject/brioche.bri", script).await;

    project_dir
}

#[tokio::test]
async fn test_check_basic_valid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        r#"
            export const project = {};
            const foo: number = 123;
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

    let result = brioche::brioche::script::check::check(&brioche, &project)
        .await?
        .ensure_ok(DiagnosticLevel::Message);

    assert_matches!(result, Ok(_));

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
            const foo: number = "123";
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

    let result = brioche::brioche::script::check::check(&brioche, &project)
        .await?
        .ensure_ok(DiagnosticLevel::Message);

    assert_matches!(result, Err(_));

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
            "brioche-repo/foo/brioche.bri",
            r#"
                export const project = {};
                export const foo: number = 123;
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project)
        .await?
        .ensure_ok(DiagnosticLevel::Message);

    assert_matches!(result, Ok(_));

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
            "brioche-repo/foo/brioche.bri",
            r#"
                export const project = {};
                export const foo: string = 123;
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let result = brioche::brioche::script::check::check(&brioche, &project)
        .await?
        .ensure_ok(DiagnosticLevel::Message);

    assert_matches!(result, Err(_));

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

    let result = brioche::brioche::script::check::check(&brioche, &project)
        .await?
        .ensure_ok(DiagnosticLevel::Message);

    assert_matches!(result, Err(_));

    Ok(())
}
