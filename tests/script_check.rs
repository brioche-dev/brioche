use std::{collections::HashMap, path::PathBuf};

use assert_matches::assert_matches;
use brioche::brioche::project::{
    resolve_project, DependencyDefinition, ProjectDefinition, Version,
};
use brioche::brioche::script::check::DiagnosticLevel;

mod brioche_test;

async fn write_project(
    context: &brioche_test::TestContext,
    name: &str,
    def: &ProjectDefinition,
    script: &str,
) -> PathBuf {
    let project_dir = context.mkdir(name).await;
    context
        .write_toml(&format!("{name}/brioche.toml"), def)
        .await;

    context.write_file("myproject/brioche.bri", script).await;

    project_dir
}

#[tokio::test]
async fn test_check_basic_valid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = write_project(
        &context,
        "myproject",
        &ProjectDefinition {
            dependencies: HashMap::new(),
        },
        r#"
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
        &ProjectDefinition {
            dependencies: HashMap::new(),
        },
        r#"
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
        &ProjectDefinition {
            dependencies: HashMap::from_iter([(
                "foo".into(),
                DependencyDefinition::Version(Version::Any),
            )]),
        },
        r#"
            import { foo } from "foo";
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
        .write_toml(
            "brioche-repo/foo/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::new(),
            },
        )
        .await;
    context
        .write_file(
            "brioche-repo/foo/brioche.bri",
            r#"
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
        &ProjectDefinition {
            dependencies: HashMap::from_iter([(
                "foo".into(),
                DependencyDefinition::Version(Version::Any),
            )]),
        },
        r#"
            import { foo } from "foo";
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
        .write_toml(
            "brioche-repo/foo/brioche.toml",
            &ProjectDefinition {
                dependencies: HashMap::new(),
            },
        )
        .await;
    context
        .write_file(
            "brioche-repo/foo/brioche.bri",
            r#"
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
        &ProjectDefinition {
            dependencies: HashMap::new(),
        },
        r#"
            import { foo } from "foo";
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
