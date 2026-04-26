use anyhow::Context as _;
use brioche_core::{
    recipe::Directory,
    script::{evaluate::evaluate, initialize_js_platform},
};

fn test_git_signature() -> gix::actor::Signature {
    gix::actor::Signature {
        name: "Brioche Test".into(),
        email: "brioche@example.com".into(),
        time: gix::date::Time {
            seconds: 0,
            offset: 0,
        },
    }
}

enum TestCommitEntry<'a> {
    File { name: &'a str, content: &'a str },
    Submodule { name: &'a str, commit: gix::hash::ObjectId },
}

fn file_repo_url(repo_dir: &std::path::Path) -> anyhow::Result<url::Url> {
    url::Url::from_directory_path(repo_dir)
        .map_err(|()| anyhow::anyhow!("failed to create file URL for {}", repo_dir.display()))
}

fn create_test_commit_with_entries(
    repo: &gix::Repository,
    entries: &[TestCommitEntry<'_>],
    parent: Option<gix::hash::ObjectId>,
    message: &str,
) -> anyhow::Result<gix::hash::ObjectId> {
    let mut entries = entries
        .iter()
        .map(|entry| match entry {
            TestCommitEntry::File { name, content } => {
                let blob_id = repo
                    .write_object(gix::objs::Blob {
                        data: content.as_bytes().to_vec(),
                    })?
                    .detach();

                anyhow::Ok(gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Blob.into(),
                    filename: (*name).into(),
                    oid: blob_id,
                })
            }
            TestCommitEntry::Submodule { name, commit } => anyhow::Ok(gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Commit.into(),
                filename: (*name).into(),
                oid: *commit,
            }),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    entries.sort();

    let tree_id = repo.write_object(gix::objs::Tree { entries })?.detach();
    let signature = test_git_signature();
    let commit_id = repo
        .write_object(gix::objs::Commit {
            tree: tree_id,
            parents: parent.into_iter().collect(),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            message: message.into(),
            extra_headers: Vec::new(),
        })?
        .detach();

    Ok(commit_id)
}

fn create_test_commit(
    repo: &gix::Repository,
    files: &[(&str, &str)],
    parent: Option<gix::hash::ObjectId>,
    message: &str,
) -> anyhow::Result<gix::hash::ObjectId> {
    let entries = files
        .iter()
        .map(|(name, content)| TestCommitEntry::File {
            name,
            content,
        })
        .collect::<Vec<_>>();
    create_test_commit_with_entries(repo, &entries, parent, message)
}

fn create_test_git_repo(
    repo_dir: &std::path::Path,
) -> anyhow::Result<(url::Url, String, String, String)> {
    let repo = gix::init_bare(repo_dir)?;

    let main_commit = create_test_commit(&repo, &[("shared.txt", "main\n")], None, "main")?;
    repo.reference(
        "refs/heads/main",
        main_commit,
        gix::refs::transaction::PreviousValue::Any,
        "test main",
    )?;

    let feature1_commit = create_test_commit(
        &repo,
        &[("feature.txt", "feature1\n"), ("shared.txt", "main\n")],
        Some(main_commit),
        "feature1",
    )?;
    repo.reference(
        "refs/heads/feature1",
        feature1_commit,
        gix::refs::transaction::PreviousValue::Any,
        "test feature1",
    )?;

    let feature2_commit = create_test_commit(
        &repo,
        &[("feature.txt", "feature2\n"), ("shared.txt", "main\n")],
        Some(main_commit),
        "feature2",
    )?;
    repo.reference(
        "refs/heads/feature2",
        feature2_commit,
        gix::refs::transaction::PreviousValue::Any,
        "test feature2",
    )?;

    let repo_url = file_repo_url(repo_dir)?;

    Ok((
        repo_url,
        main_commit.to_string(),
        feature1_commit.to_string(),
        feature2_commit.to_string(),
    ))
}

fn create_test_git_repo_with_recursive_submodules(
    repo_dir: &std::path::Path,
    submodule_repo_dir: &std::path::Path,
    nested_repo_dir: &std::path::Path,
) -> anyhow::Result<url::Url> {
    let nested_repo = gix::init_bare(nested_repo_dir)?;
    let nested_commit = create_test_commit(&nested_repo, &[("leaf.txt", "leaf\n")], None, "leaf")?;
    nested_repo.reference(
        "refs/heads/main",
        nested_commit,
        gix::refs::transaction::PreviousValue::Any,
        "test nested main",
    )?;
    let nested_repo_url = file_repo_url(nested_repo_dir)?;

    let submodule_repo = gix::init_bare(submodule_repo_dir)?;
    let submodule_gitmodules = format!(
        "[submodule \"nested\"]\n\tpath = nested\n\turl = {}\n",
        nested_repo_url
    );
    let submodule_commit = create_test_commit_with_entries(
        &submodule_repo,
        &[
            TestCommitEntry::File {
                name: ".gitmodules",
                content: &submodule_gitmodules,
            },
            TestCommitEntry::File {
                name: "module.txt",
                content: "module\n",
            },
            TestCommitEntry::Submodule {
                name: "nested",
                commit: nested_commit,
            },
        ],
        None,
        "submodule",
    )?;
    submodule_repo.reference(
        "refs/heads/main",
        submodule_commit,
        gix::refs::transaction::PreviousValue::Any,
        "test submodule main",
    )?;
    let submodule_repo_url = file_repo_url(submodule_repo_dir)?;

    let repo = gix::init_bare(repo_dir)?;
    let root_gitmodules = format!(
        "[submodule \"submodule\"]\n\tpath = submodule\n\turl = {}\n",
        submodule_repo_url
    );
    let root_commit = create_test_commit_with_entries(
        &repo,
        &[
            TestCommitEntry::File {
                name: ".gitmodules",
                content: &root_gitmodules,
            },
            TestCommitEntry::File {
                name: "root.txt",
                content: "root\n",
            },
            TestCommitEntry::Submodule {
                name: "submodule",
                commit: submodule_commit,
            },
        ],
        None,
        "root",
    )?;
    repo.reference(
        "refs/heads/main",
        root_commit,
        gix::refs::transaction::PreviousValue::Any,
        "test root main",
    )?;

    file_repo_url(repo_dir)
}

async fn single_git_checkout_dir(
    brioche: &brioche_core::Brioche,
) -> anyhow::Result<std::path::PathBuf> {
    let checkout_root = brioche.data_dir.join("git-checkouts");
    let mut entries = tokio::fs::read_dir(&checkout_root)
        .await
        .with_context(|| format!("failed to read {}", checkout_root.display()))?;
    let mut paths = vec![];
    while let Some(entry) = entries.next_entry().await? {
        paths.push(entry.path());
    }

    anyhow::ensure!(
        paths.len() == 1,
        "expected exactly one git checkout dir in {}, found {}",
        checkout_root.display(),
        paths.len()
    );

    Ok(paths.pop().expect("one path exists"))
}

#[tokio::test]
async fn test_eval_basic() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
                export default {
                    briocheSerialize: () => {
                        return {
                            type: "directory",
                            entries: {},
                        }
                    },
                };
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    assert_eq!(resolved, brioche_test_support::dir_empty().into());

    Ok(())
}

#[tokio::test]
async fn test_eval_basic_function() -> anyhow::Result<()> {
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "custom",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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
            resources: Directory::default(),
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

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
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

    let repo_dir = context.mkdir("repo").await;
    let (repo_url, _, _, _) = create_test_git_repo(&repo_dir)?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref }) => {
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                    },
                                );
                            },
                        };
                    }
                };

                export default async () => {
                    return await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                    });
                };
            "#
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    assert_eq!(
        resolved,
        brioche_test_support::dir(
            &brioche,
            [(
                "shared.txt",
                brioche_test_support::file(
                    brioche_test_support::blob(&brioche, "main\n").await,
                    false,
                ),
            )],
        )
        .await
        .into(),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_checkout_keep_git_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let repo_dir = context.mkdir("repo").await;
    let (repo_url, _, _, _) = create_test_git_repo(&repo_dir)?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref, options }) => {
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                        options,
                                    },
                                );
                            },
                        };
                    }
                };

                export default async () => {
                    return await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                        options: {
                            keepGitDir: true,
                        },
                    });
                };
            "#
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let _resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    let checkout_dir = single_git_checkout_dir(&brioche).await?;
    assert!(tokio::fs::try_exists(checkout_dir.join(".git/HEAD")).await?);

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_checkout_with_tag() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let repo_dir = context.mkdir("repo").await;
    let (repo_url, _, _, _) = create_test_git_repo(&repo_dir)?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref, options }) => {
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                        options,
                                    },
                                );
                            },
                        };
                    }
                };

                export default async () => {
                    return await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                        options: {
                            keepGitDir: true,
                            tag: true,
                        },
                    });
                };
            "#
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let _resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    let checkout_dir = single_git_checkout_dir(&brioche).await?;
    let tag_ref_path = checkout_dir.join(".git/refs/tags/main");
    assert!(tokio::fs::try_exists(&tag_ref_path).await?);
    let tag_ref = tokio::fs::read_to_string(&tag_ref_path).await?;
    assert!(!tag_ref.trim().is_empty());

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_checkout_tag_requires_keep_git_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let repo_dir = context.mkdir("repo").await;
    let (repo_url, _, _, _) = create_test_git_repo(&repo_dir)?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref, options }) => {
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                        options,
                                    },
                                );
                            },
                        };
                    }
                };

                export default async () => {
                    return await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                        options: {
                            tag: "release",
                        },
                    });
                };
            "#
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let error = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await
    .expect_err("git checkout tag without keepGitDir should fail");

    assert!(error.to_string().contains("Cannot use 'tag' without 'keepGitDir' enabled"));

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_checkout_without_submodules() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let repo_dir = context.mkdir("repo").await;
    let submodule_repo_dir = context.mkdir("submodule-repo").await;
    let nested_repo_dir = context.mkdir("nested-repo").await;
    let repo_url = create_test_git_repo_with_recursive_submodules(
        &repo_dir,
        &submodule_repo_dir,
        &nested_repo_dir,
    )?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref, options }) => {
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                        options,
                                    },
                                );
                            },
                        };
                    }
                };

                export default async () => {
                    return await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                    });
                };
            "#
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    let root_gitmodules_blob = brioche_test_support::blob(
        &brioche,
        format!(
            "[submodule \"submodule\"]\n\tpath = submodule\n\turl = {}\n",
            file_repo_url(&submodule_repo_dir)?
        ),
    )
    .await;
    let root_blob = brioche_test_support::blob(&brioche, "root\n").await;

    assert_eq!(
        resolved,
        brioche_test_support::dir(
            &brioche,
            [
                (
                    ".gitmodules",
                    brioche_test_support::file(root_gitmodules_blob, false),
                ),
                (
                    "root.txt",
                    brioche_test_support::file(root_blob, false),
                ),
                ("submodule", brioche_test_support::dir_empty()),
            ],
        )
        .await
        .into(),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_checkout_with_submodules() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let repo_dir = context.mkdir("repo").await;
    let submodule_repo_dir = context.mkdir("submodule-repo").await;
    let nested_repo_dir = context.mkdir("nested-repo").await;
    let repo_url = create_test_git_repo_with_recursive_submodules(
        &repo_dir,
        &submodule_repo_dir,
        &nested_repo_dir,
    )?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref, options }) => {
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                        options,
                                    },
                                );
                            },
                        };
                    }
                };

                export default async () => {
                    return await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                        options: {
                            submodules: true,
                        },
                    });
                };
            "#
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    let root_gitmodules_blob = brioche_test_support::blob(
        &brioche,
        format!(
            "[submodule \"submodule\"]\n\tpath = submodule\n\turl = {}\n",
            file_repo_url(&submodule_repo_dir)?
        ),
    )
    .await;
    let submodule_gitmodules_blob = brioche_test_support::blob(
        &brioche,
        format!(
            "[submodule \"nested\"]\n\tpath = nested\n\turl = {}\n",
            file_repo_url(&nested_repo_dir)?
        ),
    )
    .await;
    let root_blob = brioche_test_support::blob(&brioche, "root\n").await;
    let module_blob = brioche_test_support::blob(&brioche, "module\n").await;
    let leaf_blob = brioche_test_support::blob(&brioche, "leaf\n").await;

    let nested_dir = brioche_test_support::dir(
        &brioche,
        [(
            "leaf.txt",
            brioche_test_support::file(leaf_blob, false),
        )],
    )
    .await;
    let submodule_dir = brioche_test_support::dir(
        &brioche,
        [
            (
                ".gitmodules",
                brioche_test_support::file(submodule_gitmodules_blob, false),
            ),
            (
                "module.txt",
                brioche_test_support::file(module_blob, false),
            ),
            ("nested", nested_dir),
        ],
    )
    .await;

    assert_eq!(
        resolved,
        brioche_test_support::dir(
            &brioche,
            [
                (
                    ".gitmodules",
                    brioche_test_support::file(root_gitmodules_blob, false),
                ),
                (
                    "root.txt",
                    brioche_test_support::file(root_blob, false),
                ),
                ("submodule", submodule_dir),
            ],
        )
        .await
        .into(),
    );

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_checkout_with_submodules_keep_git_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let repo_dir = context.mkdir("repo").await;
    let submodule_repo_dir = context.mkdir("submodule-repo").await;
    let nested_repo_dir = context.mkdir("nested-repo").await;
    let repo_url = create_test_git_repo_with_recursive_submodules(
        &repo_dir,
        &submodule_repo_dir,
        &nested_repo_dir,
    )?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

                globalThis.Brioche = {
                    gitCheckout: async ({ repository, ref, options }) => {
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                        options,
                                    },
                                );
                            },
                        };
                    }
                };

                export default async () => {
                    return await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "main",
                        options: {
                            keepGitDir: true,
                            submodules: true,
                        },
                    });
                };
            "#
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let _resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    let checkout_dir = single_git_checkout_dir(&brioche).await?;
    assert!(tokio::fs::try_exists(checkout_dir.join(".git/HEAD")).await?);
    assert!(tokio::fs::try_exists(checkout_dir.join("submodule/.git/HEAD")).await?);
    assert!(tokio::fs::try_exists(checkout_dir.join("submodule/nested/.git/HEAD")).await?);

    Ok(())
}

#[tokio::test]
async fn test_eval_brioche_git_ref_and_git_checkout() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let repo_dir = context.mkdir("repo").await;
    let (repo_url, main_commit, feature1_commit, feature2_commit) =
        create_test_git_repo(&repo_dir)?;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};

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
                        return {
                            briocheSerialize: async () => {
                                return await Deno.core.ops.op_brioche_get_static(
                                    import.meta.url,
                                    {
                                        type: "git_checkout",
                                        repository,
                                        ref,
                                    },
                                );
                            },
                        };
                    },
                };

                export default async () => {
                    const gitRefMain = await Brioche.gitRef({
                        repository: "<REPO_URL>",
                        ref: "main",
                    });
                    const gitCheckoutFeat1 = await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "feature1",
                    });
                    const gitRefFeat2 = await Brioche.gitRef({
                        repository: "<REPO_URL>",
                        ref: "feature2",
                    });
                    const gitCheckoutFeat2 = await Brioche.gitCheckout({
                        repository: "<REPO_URL>",
                        ref: "feature2",
                    });
                    return {
                        briocheSerialize: async () => {
                            return {
                                type: "create_file",
                                content: JSON.stringify({
                                    gitRefMain,
                                    gitCheckoutFeat1,
                                    gitRefFeat2,
                                    gitCheckoutFeat2,
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
            .replace("<REPO_URL>", repo_url.as_str()),
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let resolved = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;

    let brioche_core::recipe::Recipe::CreateFile { content, .. } = resolved else {
        panic!("expected create_file recipe, got {resolved:?}");
    };

    let git_ref: serde_json::Value = serde_json::from_slice(&content)?;
    assert_eq!(
        git_ref,
        serde_json::json!({
            "gitRefMain": {
                "staticKind": "git_ref",
                "repository": repo_url.as_str(),
                "commit": main_commit,
            },
            "gitCheckoutFeat1": {},
            "gitRefFeat2": {
                "staticKind": "git_ref",
                "repository": repo_url.as_str(),
                "commit": feature2_commit,
            },
            "gitCheckoutFeat2": {},
        }),
    );

    assert_eq!(feature1_commit.len(), 40);

    Ok(())
}
