use brioche_core::script::evaluate::evaluate;

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
async fn test_eval_brioche_include_file() -> anyhow::Result<()> {
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
                    includeFile: (path) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "include",
                                        include: "file",
                                        path,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.includeFile("./foo");
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

#[tokio::test]
async fn test_eval_brioche_include_directory() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test::blob(&brioche, hello_world).await;
    context
        .write_file("myproject/foo/hello.txt", hello_world)
        .await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    includeDirectory: (path) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "include",
                                        include: "directory",
                                        path,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.includeDirectory("./foo");
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_test::dir(
            &brioche,
            [("hello.txt", brioche_test::file(hello_world_blob, false))]
        )
        .await
        .into(),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_glob() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test::blob(&brioche, hello_world).await;
    context
        .write_file("myproject/foo/hello.txt", hello_world)
        .await;

    let hi = "hello world!";
    let hi_blob = brioche_test::blob(&brioche, hi).await;
    context.write_file("myproject/bar/hi.txt", hi).await;

    context
        .write_file("myproject/bar/secret.md", "not included!")
        .await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    glob: (...patterns) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "glob",
                                        patterns,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.glob("foo", "bar/**/*.txt");
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_test::dir(
            &brioche,
            [
                (
                    "foo",
                    brioche_test::dir(
                        &brioche,
                        [("hello.txt", brioche_test::file(hello_world_blob, false))]
                    )
                    .await,
                ),
                (
                    "bar",
                    brioche_test::dir(&brioche, [("hi.txt", brioche_test::file(hi_blob, false))])
                        .await,
                ),
            ],
        )
        .await
        .into(),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_glob_submodule() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test::blob(&brioche, hello_world).await;
    context
        .write_file("myproject/foo/hello.txt", hello_world)
        .await;
    context
        .write_file("myproject/foo/fizz/buzz.txt", hello_world)
        .await;

    context
        .write_file("myproject/bar/hi.txt", "outside of submodule")
        .await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { foo } from "./foo";

                export default () => {
                    return foo();
                };
            "#,
        )
        .await;

    context
        .write_file(
            "myproject/foo/index.bri",
            r#"
                globalThis.Brioche = {
                    glob: (...patterns) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "glob",
                                        patterns,
                                    },
                                );
                            },
                        };
                    }
                }

                export function foo() {
                    return Brioche.glob("**/*.txt");
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_test::dir(
            &brioche,
            [
                ("hello.txt", brioche_test::file(hello_world_blob, false)),
                (
                    "fizz",
                    brioche_test::dir(
                        &brioche,
                        [("buzz.txt", brioche_test::file(hello_world_blob, false))]
                    )
                    .await,
                ),
            ],
        )
        .await
        .into(),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_download() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;
    let hello_hash = brioche_test::sha256(hello);
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body(hello)
        .expect(1)
        .create();

    let download_url = format!("{server_url}/file.txt");

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                globalThis.Brioche = {
                    download: (url) => {
                        return {
                            briocheSerialize: async () => {
                                return Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "download",
                                        url,
                                    },
                                );
                            },
                        };
                    }
                }

                export default () => {
                    return Brioche.download("<DOWNLOAD_URL>");
                };
            "#
            .replace("<DOWNLOAD_URL>", &download_url),
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_core::recipe::Recipe::Download(brioche_core::recipe::DownloadRecipe {
            url: download_url.parse().unwrap(),
            hash: hello_hash,
        })
    );

    // Bake the download, which ensures that the download was cached
    let baked = brioche_test::bake_without_meta(&brioche, resolved).await?;
    assert_eq!(
        baked,
        brioche_core::recipe::Artifact::File(brioche_core::recipe::File {
            content_blob: hello_blob,
            executable: false,
            resources: Default::default(),
        })
    );

    hello_endpoint.assert_async().await;

    Ok(())
}
