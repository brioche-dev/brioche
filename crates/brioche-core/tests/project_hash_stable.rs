#[tokio::test]
async fn test_project_hash_stable_simple() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            indoc::indoc! {r"
                export const project = {};
            "},
        )
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &project_dir).await;
    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();

    assert_eq!(
        project_hash.to_string(),
        "2f320d70fbfa08613479b47f4cdb5ef6a50f491b3612df7bb3fa2f4089b80895",
    );
}

#[tokio::test]
async fn test_project_hash_stable_simple_no_definition() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r"").await;

    let project_ref = brioche_test_support::load_project(brioche, &project_dir).await;
    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();

    assert_eq!(
        project_hash.to_string(),
        "0c5d6dcbd231292f3bc02e07154c52bd2b162ec61dd82d1ff2af08ba7e3821bf",
    );
}
#[tokio::test]
async fn test_project_hash_stable_workspace_dep() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./foo".parse().unwrap(), "./bar".parse().unwrap()],
            },
        )
        .await;

    context.mkdir("myworkspace/foo").await;
    context
        .write_file(
            "myworkspace/foo/project.bri",
            indoc::indoc! {r"
                // Workspace foo
            "},
        )
        .await;

    let project_dir = context.mkdir("myworkspace/myproject").await;
    context
        .write_file(
            "myworkspace/myproject/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#},
        )
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &project_dir).await;
    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();

    let foo_project_ref = brioche.projects().project_dependencies(project_ref)["foo"];
    let foo_project_hash = brioche_core::project::hash::hash_project(brioche, foo_project_ref)
        .await
        .unwrap();

    assert_eq!(
        project_hash.to_string(),
        "1056fe1c9deca5a68ce1630bd128d1027bba36ff99bcafee248948a16017c139",
    );
    assert_eq!(
        foo_project_hash.to_string(),
        "4e859a6ee3a807f5bfab73b6a90a50b5fa73be59d79c127df505f6422d79d938",
    );
}

#[tokio::test]
async fn test_project_hash_stable_path_dep() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_file(
            "mainproject/project.bri",
            indoc::indoc! {r#"
                import "depproject";
                export const project = {
                    dependencies: {
                        depproject: {
                            path: "../depproject",
                        },
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "depproject/project.bri",
            indoc::indoc! {r"
                export const project = {};
            "},
        )
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &main_project_dir).await;
    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();
    let dep_project_ref = brioche.projects().project_dependencies(project_ref)["depproject"];
    let dep_project_hash = brioche_core::project::hash::hash_project(brioche, dep_project_ref)
        .await
        .unwrap();

    assert_eq!(
        dep_project_hash.to_string(),
        "2f320d70fbfa08613479b47f4cdb5ef6a50f491b3612df7bb3fa2f4089b80895"
    );
    assert_eq!(
        project_hash.to_string(),
        "340118156a9c59ab6a5f66bdd81fb81bd698bc12accec09f107954eb0c9a96b1"
    );
}

#[tokio::test]
async fn test_project_hash_stable_registry_dep() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;
    let brioche = &mut *brioche.write().await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                indoc::indoc! {r"
                    export const project = {};
                "},
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#},
        )
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &project_dir).await;
    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();

    mock_foo_latest.assert_async().await;

    assert_eq!(
        foo_hash.to_string(),
        "2f320d70fbfa08613479b47f4cdb5ef6a50f491b3612df7bb3fa2f4089b80895",
    );
    assert_eq!(
        project_hash.to_string(),
        "7b69568d464d3a9fe7fbc8e432396f914f9636f4f88c02a6b11becc8694beffd",
    );
}

#[tokio::test]
async fn test_project_hash_stable_registry_dep_with_brioche_include() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;
    let brioche = &mut *brioche.write().await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::write(path.join("fizz"), "fizz!").await.unwrap();
            tokio::fs::create_dir_all(path.join("buzz/hello"))
                .await
                .unwrap();
            tokio::fs::write(path.join("buzz/hello/world.txt"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(
                path.join("project.bri"),
                indoc::indoc! {r#"
                    export const project = {
                        name: "foo",
                    };

                    export const foo = Brioche.includeFile("fizz");
                    export const bar = Brioche.includeDirectory("buzz");
                "#},
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#},
        )
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &project_dir).await;
    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();

    mock_foo_latest.assert_async().await;

    assert_eq!(
        project_hash.to_string(),
        "8fcd48e9ba65c61ee3faec2d3498bf4b3fbfa5c7ed144b1d94f4efd14953ed40",
    );
    assert_eq!(
        foo_hash.to_string(),
        "0ca26b3e43efa29a16ae7e9562dfe1ef9b91d4dddf2a9d720cd196444cf6351a",
    );
}

#[tokio::test]
async fn test_project_hash_stable_registry_dep_with_brioche_glob() {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;
    let brioche = &mut *brioche.write().await;

    let foo_hash = context
        .cached_registry_project(&cache, async |path| {
            tokio::fs::create_dir_all(path.join("fizz")).await.unwrap();
            tokio::fs::write(path.join("fizz/hello.md"), "fizz!")
                .await
                .unwrap();
            tokio::fs::create_dir_all(path.join("buzz")).await.unwrap();
            tokio::fs::write(path.join("buzz/hello.txt"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(path.join("buzz/hello.secret"), "buzz!")
                .await
                .unwrap();
            tokio::fs::write(
                path.join("project.bri"),
                indoc::indoc! {r#"
                    export const project = {
                        name: "foo",
                    };

                    export const globbed = Brioche.glob("fizz", "**/*.txt");
                "#},
            )
            .await
            .unwrap();
        })
        .await;
    let mock_foo_latest = context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#},
        )
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &project_dir).await;
    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();

    mock_foo_latest.assert_async().await;

    assert_eq!(
        project_hash.to_string(),
        "fc581645b65bedb6b8469e2678bde474951445371a37466137e2368e53c02c5e",
    );
    assert_eq!(
        foo_hash.to_string(),
        "f59790225a71cfc3d2691015752545f3de1ef1a78ab9d7fc9259f8b2cb8620c1",
    );
}

#[tokio::test]
async fn test_project_hash_stable_remote_workspace_registry_dep() {
    let cache = brioche_test_support::new_cache();
    let bar_hash;

    {
        let (brioche, context) =
            brioche_test_support::brioche_test_with_cache(cache.clone(), true).await;
        let brioche = &mut *brioche.write().await;

        context
            .write_toml(
                "myworkspace/brioche_workspace.toml",
                &brioche_core::project::WorkspaceDefinition {
                    members: vec!["./foo".parse().unwrap(), "./bar".parse().unwrap()],
                },
            )
            .await;

        let foo_dir = context.mkdir("myworkspace/foo").await;
        context
            .write_file("myworkspace/foo/project.bri", "export const project = {};")
            .await;

        let bar_dir = context.mkdir("myworkspace/bar").await;
        context
            .write_file(
                "myworkspace/bar/project.bri",
                indoc::indoc! {r#"
                    export const project = {
                        dependencies: {
                            foo: "*",
                        },
                    };
                "#},
            )
            .await;

        let bar_ref = brioche_test_support::load_project(brioche, &bar_dir).await;
        let foo_ref = brioche_test_support::load_project(brioche, &foo_dir).await;

        let bar_deps = brioche.projects().project_dependencies(bar_ref);
        assert_eq!(
            bar_deps.len(),
            1,
            "expected bar to have 1 dependency, got: {bar_deps:#?}"
        );
        assert_eq!(bar_deps["foo"], foo_ref);

        bar_hash = brioche_core::project::hash::hash_project(brioche, bar_ref)
            .await
            .expect("failed to hash bar project");
        let bar_project_artifact =
            brioche_core::project::artifact::create_project_artifact(brioche, bar_ref)
                .await
                .expect("failed to create artifact for bar");
        let bar_project_artifact_hash =
            brioche_core::recipe::hash::hash_recipe(brioche, bar_project_artifact);
        brioche_core::cache::save_artifact(brioche, bar_project_artifact)
            .await
            .expect("failed to save bar project artifact to cache");
        brioche_core::cache::save_project_artifact_hash(
            brioche,
            bar_hash,
            bar_project_artifact_hash,
        )
        .await
        .expect("failed to save bar project artifact hash to cache");
    }

    let (brioche, mut context) =
        brioche_test_support::brioche_test_with_cache(cache.clone(), false).await;
    let brioche = &mut *brioche.write().await;

    let mock_bar_latest = context
        .mock_registry_publish_tag("bar", "latest", bar_hash)
        .create_async()
        .await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        bar: "*",
                    },
                };
            "#},
        )
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &project_dir).await;
    let bar_ref = brioche.projects().project_dependencies(project_ref)["bar"];
    let foo_ref = brioche.projects().project_dependencies(bar_ref)["foo"];

    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();
    let bar_hash = brioche_core::project::hash::hash_project(brioche, bar_ref)
        .await
        .expect("failed to hash bar");
    let foo_hash = brioche_core::project::hash::hash_project(brioche, foo_ref)
        .await
        .expect("failed to hash foo");

    mock_bar_latest.assert_async().await;

    assert_eq!(
        project_hash.to_string(),
        "f7e76ca70957c980526dffce3c5b6b0ca0908904063c185546a89320ac1fbab3",
    );
    assert_eq!(
        bar_hash.to_string(),
        "74aee30d79306c2bb2498e8819f2344e235178b40ad832a78f1bc30f372e01fc",
    );
    assert_eq!(
        foo_hash.to_string(),
        "bcdfa515f4de0edb50aeb368df76bdc4d0937321f0ff38b520ad150057549eee",
    );
}

#[tokio::test]
async fn test_project_hash_stable_complex() {
    let (brioche, mut context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_file(
            "mainproject/project.bri",
            indoc::indoc! {r#"
                import "depproject";
                import "foo";

                export const project = {
                    dependencies: {
                        depproject: {
                            path: "../depproject",
                        },
                    },
                };
            "#},
        )
        .await;

    context.mkdir("depproject").await;
    context
        .write_file(
            "depproject/project.bri",
            indoc::indoc! {r#"
                import "foo";
            "#},
        )
        .await;

    let (bar_hash, _) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                indoc::indoc! {r"
                    // Empty project
                "},
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("bar", "latest", bar_hash)
        .create_async()
        .await;

    let (foo_hash, _) = context
        .local_registry_project(async |path| {
            tokio::fs::write(
                path.join("project.bri"),
                indoc::indoc! {r#"
                    import "bar";
                "#},
            )
            .await
            .unwrap();
        })
        .await;
    context
        .mock_registry_publish_tag("foo", "latest", foo_hash)
        .create_async()
        .await;

    let project_ref = brioche_test_support::load_project(brioche, &main_project_dir).await;

    let project_hash = brioche_core::project::hash::hash_project(brioche, project_ref)
        .await
        .unwrap();

    assert_eq!(
        project_hash.to_string(),
        "b249591e6e1644a6cb98b74e8ec531f3177a6c7281c684e70330f37f0e61a391",
    );
}

#[tokio::test]
async fn test_project_hash_stable_cyclic_simple() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./alpha".parse().unwrap(), "./beta".parse().unwrap()],
            },
        )
        .await;

    let alpha_project_dir = context.mkdir("myworkspace/alpha").await;
    context
        .write_file(
            "myworkspace/alpha/project.bri",
            indoc::indoc! {r#"
                import { beta } from "beta";
                export const alpha = "alpha";
            "#},
        )
        .await;

    context
        .write_file(
            "myworkspace/beta/project.bri",
            indoc::indoc! {r#"
                import { alpha } from "alpha";
                export const beta = "beta";
            "#},
        )
        .await;

    let alpha_project_ref = brioche_test_support::load_project(brioche, &alpha_project_dir).await;
    let alpha_project_hash = brioche_core::project::hash::hash_project(brioche, alpha_project_ref)
        .await
        .unwrap();

    let beta_project_ref = brioche.projects().project_dependencies(alpha_project_ref)["beta"];
    let beta_project_hash = brioche_core::project::hash::hash_project(brioche, beta_project_ref)
        .await
        .unwrap();

    assert_eq!(
        alpha_project_hash.to_string(),
        "d8513bfa27a2559370ed1541276b93860f0b70c7ff631ac153e331bdb1d19727",
    );
    assert_eq!(
        beta_project_hash.to_string(),
        "2ed0c5dcf69a7bb80d0490a954d9676f4e3d871a186d57d10e2425d2a91dc22b",
    );
}

#[tokio::test]
async fn test_project_hash_stable_cyclic_complex() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    // Project structure:
    //
    // main     -> foo/a1
    // foo/a1   -> foo/a2
    // foo/a2   -> foo/a3
    // foo/a3   -> foo/a1, bar/b
    // bar/b    -> bar/c1
    // bar/c1   -> bar/c2, bar/c3, bar/d
    // bar/c2   -> bar/c1, bar/c3, bar/d
    // bar/c3   -> bar/c1, bar/c2, bar/d
    // bar/d    -> bar/e1
    // bar/e1   -> bar/e2
    // bar/e2   -> bar/e1, bar/f
    // bar/f    -> baz/g
    // baz/g    -> baz/h
    // baz/h -> (no dependencies)

    let main_project_dir = context.mkdir("main").await;
    context
        .write_file(
            "main/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        a1: {
                            path: "../foo/a1",
                        },
                    },
                };
            "#},
        )
        .await;

    context
        .write_toml(
            "foo/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec![
                    "./a1".parse().unwrap(),
                    "./a2".parse().unwrap(),
                    "./a3".parse().unwrap(),
                ],
            },
        )
        .await;

    context
        .write_file(
            "foo/a1/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        a2: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "foo/a2/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        a3: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "foo/a3/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        a1: "*",
                        b: {
                            path: "../../bar/b",
                        },
                    },
                };
            "#},
        )
        .await;

    context
        .write_toml(
            "bar/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec![
                    "./b".parse().unwrap(),
                    "./c1".parse().unwrap(),
                    "./c2".parse().unwrap(),
                    "./c3".parse().unwrap(),
                    "./d".parse().unwrap(),
                    "./e1".parse().unwrap(),
                    "./e2".parse().unwrap(),
                    "./f".parse().unwrap(),
                ],
            },
        )
        .await;

    context
        .write_file(
            "bar/b/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        c1: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "bar/c1/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        c2: "*",
                        c3: "*",
                        d: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "bar/c2/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        c1: "*",
                        c3: {
                            path: "../c3",
                        },
                        d: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "bar/c3/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        c1: {
                            path: "../c1",
                        },
                        c2: "*",
                        d: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "bar/d/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        e1: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "bar/e1/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        e2: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "bar/e2/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        e1: "*",
                        f: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "bar/f/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        g: {
                            path: "../../baz/g",
                        },
                    },
                };
            "#},
        )
        .await;

    context
        .write_toml(
            "baz/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./g".parse().unwrap(), "./h".parse().unwrap()],
            },
        )
        .await;

    context
        .write_file(
            "baz/g/project.bri",
            indoc::indoc! {r#"
                export const project = {
                    dependencies: {
                        h: "*",
                    },
                };
            "#},
        )
        .await;

    context
        .write_file(
            "baz/h/project.bri",
            indoc::indoc! {r"
                // Empty project
            "},
        )
        .await;

    let main_project_ref = brioche_test_support::load_project(brioche, &main_project_dir).await;
    let main_project_hash = brioche_core::project::hash::hash_project(brioche, main_project_ref)
        .await
        .unwrap();

    assert_eq!(
        main_project_hash.to_string(),
        "7abb724060a67475b37f6d7c87215936870de9c904c354fd356a196399654a1e",
    );
}
