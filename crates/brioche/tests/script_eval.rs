use assert_matches::assert_matches;
use brioche::script::evaluate::evaluate;

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
                                entries: {},
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

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
                                entries: {},
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "custom")
        .await?
        .value;

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
                                entries: {},
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

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
                                entries: {},
                            }
                        },
                    };
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

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

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_import_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test::brioche_test().await;

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

    let (foo_hash, _) = context
        .local_registry_project(|path| async move {
            tokio::fs::write(
                path.join("project.bri"),
                r#"
                    export const project = {};
                    export const build = async () => {
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
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_get() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test::blob(&brioche, hello_world).await;
    context.write_file("myproject/foo", hello_world).await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    get: (path) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "get",
                                        path,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.get("./foo");
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test::file(hello_world_blob, false).into());

    Ok(())
}
