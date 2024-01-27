use brioche::brioche::{project::resolve_project, script::evaluate::evaluate};

mod brioche_test;

#[tokio::test]
async fn test_eval_basic() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
                export default () => {
                    return {
                        briocheSerialize: () => {
                            return {
                                type: "directory",
                                tree: null,
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &project, "default").await?.value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_custom_export() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
                export const custom = () => {
                    return {
                        briocheSerialize: () => {
                            return {
                                type: "directory",
                                tree: null,
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &project, "custom").await?.value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_async() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
                export default async () => {
                    return {
                        briocheSerialize: () => {
                            return {
                                type: "directory",
                                tree: null,
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &project, "default").await?.value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_serialize_async() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
                export default async () => {
                    return {
                        briocheSerialize: async () => {
                            return {
                                type: "directory",
                                tree: null,
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &project, "default").await?.value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_import_local() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/build.bri",
            r#"
                export const build = async () => {
                    return {
                        briocheSerialize: () => {
                            return {
                                type: "directory",
                                tree: null,
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { build } from "./build.bri";
                export const project = {};
                export default async () => {
                    return build();
                };
                "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &project, "default").await?.value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_import_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { build } from "foo";
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
                export default async () => {
                    return build();
                };
            "#,
        )
        .await;

    context
        .write_file(
            "brioche-repo/foo/project.bri",
            r#"
                export const project = {};
                export const build = async () => {
                    return {
                        briocheSerialize: () => {
                            return {
                                type: "directory",
                                tree: null,
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let project = resolve_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &project, "default").await?.value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}
