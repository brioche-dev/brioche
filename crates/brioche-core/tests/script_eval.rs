use brioche_core::script::evaluate::evaluate;

#[tokio::test]
async fn test_eval_basic() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test_support::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_custom_export() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "custom")
        .await?
        .value;

    assert_eq!(resolved, brioche_test_support::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_async() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test_support::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_serialize_async() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test_support::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_import_local() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test_support::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_import_dep() -> anyhow::Result<()> {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;

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
        .local_registry_project(async |path| {
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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(resolved, brioche_test_support::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_include_file() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test_support::blob(&brioche, hello_world).await;
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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_test_support::file(hello_world_blob, false).into()
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_include_directory() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test_support::blob(&brioche, hello_world).await;
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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_test_support::dir(
            &brioche,
            [(
                "hello.txt",
                brioche_test_support::file(hello_world_blob, false)
            )]
        )
        .await
        .into(),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_glob() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test_support::blob(&brioche, hello_world).await;
    context
        .write_file("myproject/foo/hello.txt", hello_world)
        .await;

    let hi = "hello world!";
    let hi_blob = brioche_test_support::blob(&brioche, hi).await;
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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_test_support::dir(
            &brioche,
            [
                (
                    "foo",
                    brioche_test_support::dir(
                        &brioche,
                        [(
                            "hello.txt",
                            brioche_test_support::file(hello_world_blob, false)
                        )]
                    )
                    .await,
                ),
                (
                    "bar",
                    brioche_test_support::dir(
                        &brioche,
                        [("hi.txt", brioche_test_support::file(hi_blob, false))]
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
async fn test_eval_brioche_glob_submodule() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    let hello_world = "hello world!";
    let hello_world_blob = brioche_test_support::blob(&brioche, hello_world).await;
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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    assert_eq!(
        resolved,
        brioche_test_support::dir(
            &brioche,
            [
                (
                    "hello.txt",
                    brioche_test_support::file(hello_world_blob, false)
                ),
                (
                    "fizz",
                    brioche_test_support::dir(
                        &brioche,
                        [(
                            "buzz.txt",
                            brioche_test_support::file(hello_world_blob, false)
                        )]
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
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let mut server = mockito::Server::new_async().await;
    let server_url = server.url();

    let hello = "hello";
    let hello_blob = brioche_test_support::blob(&brioche, hello).await;
    let hello_hash = brioche_test_support::sha256(hello);
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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

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
    let baked = brioche_test_support::bake_without_meta(&brioche, resolved).await?;
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

#[tokio::test]
async fn test_eval_brioche_git_ref() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let mut mock_repo = mockito::Server::new_async().await;
    let mock_repo_url = mock_repo.url();

    // Mock a git "handshake" server response for protocol version 2
    let mock_git_info_refs_response = b"001e# service=git-upload-pack\n0000000eversion 2\n0000";
    let mock_git_info_refs = mock_repo
        .mock("GET", "/info/refs?service=git-upload-pack")
        .with_header(
            "Content-Type",
            "application/x-git-upload-pack-advertisement",
        )
        .with_header("Cache-Control", "no-cache")
        .with_body(mock_git_info_refs_response)
        .expect(1)
        .create();

    // Mock a git "ls-refs" response, with one branch named "main" with a
    // commit hash of "0123456789abcdef01234567890123456789abcd"
    let mock_git_upload_pack_response =
        b"003d0123456789abcdef01234567890123456789abcd refs/heads/main\n0000";
    let mock_git_upload_pack = mock_repo
        .mock("POST", "/git-upload-pack")
        .with_header("Content-Type", "application/x-git-upload-pack-result")
        .with_header("Cache-Control", "no-cache")
        .with_body(mock_git_upload_pack_response)
        .expect(1)
        .create();

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                globalThis.Brioche = {
                    gitRef: async ({ repository, ref }) => {
                        return await Deno.core.ops.op_brioche_get_static(
                            import.meta.url,
                            {
                                type: "git_ref",
                                repository,
                                ref,
                            },
                        );
                    }
                }

                export default async () => {
                    const gitRef = await Brioche.gitRef({
                        repository: "<REPO_URL>",
                        ref: "main",
                    });
                    return {
                        briocheSerialize: async () => {
                            return {
                                type: "create_file",
                                content: JSON.stringify(gitRef),
                                executable: false,
                                resources: {
                                    type: "directory",
                                    entries: {},
                                },
                            };
                        },
                    };
                };
            "#
            .replace("<REPO_URL>", &mock_repo_url),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    let brioche_core::recipe::Recipe::CreateFile { content, .. } = resolved else {
        panic!("expected create_file recipe, got {resolved:?}");
    };

    mock_git_info_refs.assert_async().await;
    mock_git_upload_pack.assert_async().await;

    let git_ref: serde_json::Value = serde_json::from_slice(&content)?;
    assert_eq!(
        git_ref,
        serde_json::json!({
            "staticKind": "git_ref",
            "repository": format!("{mock_repo_url}/"),
            "commit": "0123456789abcdef01234567890123456789abcd",
        }),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_checkout() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let mut mock_repo = mockito::Server::new_async().await;
    let mock_repo_url = mock_repo.url();

    // Mock a git "handshake" server response for protocol version 2
    let mock_git_info_refs_response = b"001e# service=git-upload-pack\n0000000eversion 2\n0000";
    let mock_git_info_refs = mock_repo
        .mock("GET", "/info/refs?service=git-upload-pack")
        .with_header(
            "Content-Type",
            "application/x-git-upload-pack-advertisement",
        )
        .with_header("Cache-Control", "no-cache")
        .with_body(mock_git_info_refs_response)
        .expect(1)
        .create();

    // Mock a git "ls-refs" response, with one branch named "main" with a
    // commit hash of "0123456789abcdef01234567890123456789abcd"
    let mock_git_upload_pack_response =
        b"003d0123456789abcdef01234567890123456789abcd refs/heads/main\n0000";
    let mock_git_upload_pack = mock_repo
        .mock("POST", "/git-upload-pack")
        .with_header("Content-Type", "application/x-git-upload-pack-result")
        .with_header("Cache-Control", "no-cache")
        .with_body(mock_git_upload_pack_response)
        .expect(1)
        .create();

    // From Brioche's perspective, `Brioche.gitCheckout()` is treated
    // the same as `Brioche.gitRef()`. In practice, it will actually checkout
    // the ref, but that's implemented at the packaging layer
    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref }) => {
                        return await Deno.core.ops.op_brioche_get_static(
                            import.meta.url,
                            {
                                type: "git_ref",
                                repository,
                                ref,
                            },
                        );
                    }
                }

                export default async () => {
                    const gitCheckout = await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                    });
                    return {
                        briocheSerialize: async () => {
                            return {
                                type: "create_file",
                                content: JSON.stringify(gitCheckout),
                                executable: false,
                                resources: {
                                    type: "directory",
                                    entries: {},
                                },
                            };
                        },
                    };
                };
            "#
            .replace("<REPO_URL>", &mock_repo_url),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    let brioche_core::recipe::Recipe::CreateFile { content, .. } = resolved else {
        panic!("expected create_file recipe, got {resolved:?}");
    };

    mock_git_info_refs.assert_async().await;
    mock_git_upload_pack.assert_async().await;

    let git_ref: serde_json::Value = serde_json::from_slice(&content)?;
    assert_eq!(
        git_ref,
        serde_json::json!({
            "staticKind": "git_ref",
            "repository": format!("{mock_repo_url}/"),
            "commit": "0123456789abcdef01234567890123456789abcd",
        }),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_ref_and_git_checkout() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let mut mock_repo = mockito::Server::new_async().await;
    let mock_repo_url = mock_repo.url();

    // Mock a git "handshake" server response for protocol version 2
    // TODO: Update code so each repo only gets evaluated once
    let mock_git_info_refs_response = b"001e# service=git-upload-pack\n0000000eversion 2\n0000";
    let mock_git_info_refs = mock_repo
        .mock("GET", "/info/refs?service=git-upload-pack")
        .with_header(
            "Content-Type",
            "application/x-git-upload-pack-advertisement",
        )
        .with_header("Cache-Control", "no-cache")
        .with_body(mock_git_info_refs_response)
        .expect_at_least(1)
        .create();

    // Mock a git "ls-refs" response, with three branches:
    // - "main" with commit hash "0123456789abcdef01234567890123456789abcd"
    // - "feature1" with commit hash "abcdef0123456789abcdef012345678901234567"
    // - "feature2" with commit hash "0000000000111111111122222222223333333333"
    let mock_git_upload_pack_response =
        b"003d0123456789abcdef01234567890123456789abcd refs/heads/main\n0041abcdef0123456789abcdef012345678901234567 refs/heads/feature1\n00410000000000111111111122222222223333333333 refs/heads/feature2\n0000";
    let mock_git_upload_pack = mock_repo
        .mock("POST", "/git-upload-pack")
        .with_header("Content-Type", "application/x-git-upload-pack-result")
        .with_header("Cache-Control", "no-cache")
        .with_body(mock_git_upload_pack_response)
        .expect_at_least(1)
        .create();

    // From Brioche's perspective, `Brioche.gitCheckout()` is treated
    // the same as `Brioche.gitRef()`. In practice, it will actually checkout
    // the ref, but that's implemented at the packaging layer
    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                globalThis.Brioche = {
                    gitRef: async ({ repository, ref }) => {
                        return await Deno.core.ops.op_brioche_get_static(
                            import.meta.url,
                            {
                                type: "git_ref",
                                repository,
                                ref,
                            },
                        );
                    },
                    gitCheckout: async ({ repository, ref }) => {
                        return await Deno.core.ops.op_brioche_get_static(
                            import.meta.url,
                            {
                                type: "git_ref",
                                repository,
                                ref,
                            },
                        );
                    },
                };

                export default async () => {
                    const gitRefMain = await Brioche.gitRef({
                        repository: "<REPO_URL>",
                        ref: "main",
                    });
                    const gitCheckoutFeature1 = await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "feature1",
                    });
                    const gitRefFeature2 = await Brioche.gitRef({
                        repository: "<REPO_URL>",
                        ref: "feature2",
                    });
                    const gitCheckoutFeature2 = await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "feature2",
                    });
                    return {
                        briocheSerialize: async () => {
                            return {
                                type: "create_file",
                                content: JSON.stringify({
                                    gitRefMain,
                                    gitCheckoutFeature1,
                                    gitRefFeature2,
                                    gitCheckoutFeature2,
                                }),
                                executable: false,
                                resources: {
                                    type: "directory",
                                    entries: {},
                                },
                            };
                        },
                    };
                };
            "#
            .replace("<REPO_URL>", &mock_repo_url),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;

    let brioche_core::recipe::Recipe::CreateFile { content, .. } = resolved else {
        panic!("expected create_file recipe, got {resolved:?}");
    };

    mock_git_info_refs.assert_async().await;
    mock_git_upload_pack.assert_async().await;

    let git_ref: serde_json::Value = serde_json::from_slice(&content)?;
    assert_eq!(
        git_ref,
        serde_json::json!({
            "gitRefMain": {
                "staticKind": "git_ref",
                "repository": format!("{mock_repo_url}/"),
                "commit": "0123456789abcdef01234567890123456789abcd",
            },
            "gitCheckoutFeature1": {
                "staticKind": "git_ref",
                "repository": format!("{mock_repo_url}/"),
                "commit": "abcdef0123456789abcdef012345678901234567",
            },
            "gitRefFeature2": {
                "staticKind": "git_ref",
                "repository": format!("{mock_repo_url}/"),
                "commit": "0000000000111111111122222222223333333333",
            },
            "gitCheckoutFeature2": {
                "staticKind": "git_ref",
                "repository": format!("{mock_repo_url}/"),
                "commit": "0000000000111111111122222222223333333333",
            },
        }),
    );

    Ok(())
}
