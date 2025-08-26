use std::sync::Arc;

use assert_matches::assert_matches;
use brioche_core::{Brioche, project::ProjectEntry};
use brioche_test_support::TestContext;
use relative_path::RelativePathBuf;

#[tokio::test]
async fn test_project_load_cyclic_simple_by_path() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./alpha".parse()?, "./beta".parse()?],
            },
        )
        .await;

    let alpha_project_dir = context.mkdir("myworkspace/alpha").await;
    context
        .write_file(
            "myworkspace/alpha/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        beta: {
                            path: "../beta",
                        },
                    },
                };
            "#,
        )
        .await;

    let beta_project_dir = context.mkdir("myworkspace/beta").await;
    context
        .write_file(
            "myworkspace/beta/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        alpha: {
                            path: "../alpha",
                        },
                    },
                };
            "#,
        )
        .await;

    let (projects, alpha_project_hash) =
        brioche_test_support::load_project(&brioche, &alpha_project_dir).await?;
    let alpha_project_entry = projects.project_entry(alpha_project_hash).unwrap();
    let ProjectEntry::WorkspaceMember {
        workspace: alpha_workspace_hash,
        path: alpha_member_path,
    } = alpha_project_entry
    else {
        panic!("expected alpha_project to be a WorkspaceMember entry");
    };

    let beta_project_hash = projects.project_dependencies(alpha_project_hash).unwrap()["beta"];
    let beta_project_entry = projects.project_entry(beta_project_hash).unwrap();
    let ProjectEntry::WorkspaceMember {
        workspace: beta_workspace_hash,
        path: beta_member_path,
    } = beta_project_entry
    else {
        panic!("expected beta_project to be a WorkspaceMember entry");
    };

    // Both projects should be in the same workspace
    assert_eq!(alpha_workspace_hash, beta_workspace_hash);

    let beta_alpha_project_hash =
        projects.project_dependencies(beta_project_hash).unwrap()["alpha"];
    assert_eq!(alpha_project_hash, beta_alpha_project_hash);

    // Paths should be relative to the workspace root
    assert_eq!(
        alpha_member_path,
        RelativePathBuf::from("alpha".to_string())
    );
    assert_eq!(beta_member_path, RelativePathBuf::from("beta".to_string()));

    assert!(
        projects
            .local_paths(alpha_project_hash)
            .unwrap()
            .contains(&alpha_project_dir)
    );
    assert!(
        projects
            .local_paths(beta_project_hash)
            .unwrap()
            .contains(&beta_project_dir)
    );

    Ok(())
}

#[tokio::test]
async fn test_project_load_cyclic_simple_implied() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./alpha".parse()?, "./beta".parse()?],
            },
        )
        .await;

    let alpha_project_dir = context.mkdir("myworkspace/alpha").await;
    context
        .write_file(
            "myworkspace/alpha/project.bri",
            r#"
                import { beta } from "beta";
                export const alpha = "alpha";
            "#,
        )
        .await;

    let beta_project_dir = context.mkdir("myworkspace/beta").await;
    context
        .write_file(
            "myworkspace/beta/project.bri",
            r#"
                import { alpha } from "alpha";
                export const beta = "beta";
            "#,
        )
        .await;

    let (projects, alpha_project_hash) =
        brioche_test_support::load_project(&brioche, &alpha_project_dir).await?;
    let alpha_project_entry = projects.project_entry(alpha_project_hash).unwrap();
    let ProjectEntry::WorkspaceMember {
        workspace: alpha_workspace_hash,
        path: alpha_member_path,
    } = alpha_project_entry
    else {
        panic!("expected alpha_project to be a WorkspaceMember entry");
    };

    let beta_project_hash = projects.project_dependencies(alpha_project_hash).unwrap()["beta"];
    let beta_project_entry = projects.project_entry(beta_project_hash).unwrap();
    let ProjectEntry::WorkspaceMember {
        workspace: beta_workspace_hash,
        path: beta_member_path,
    } = beta_project_entry
    else {
        panic!("expected beta_project to be a WorkspaceMember entry");
    };

    // Both projects should be in the same workspace
    assert_eq!(alpha_workspace_hash, beta_workspace_hash);

    let beta_alpha_project_hash =
        projects.project_dependencies(beta_project_hash).unwrap()["alpha"];
    assert_eq!(alpha_project_hash, beta_alpha_project_hash);

    // Paths should be relative to the workspace root
    assert_eq!(
        alpha_member_path,
        RelativePathBuf::from("alpha".to_string())
    );
    assert_eq!(beta_member_path, RelativePathBuf::from("beta".to_string()));

    assert!(
        projects
            .local_paths(alpha_project_hash)
            .unwrap()
            .contains(&alpha_project_dir)
    );
    assert!(
        projects
            .local_paths(beta_project_hash)
            .unwrap()
            .contains(&beta_project_dir)
    );

    Ok(())
}

#[tokio::test]
async fn test_project_load_cyclic_complex() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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
            r#"
                export const project = {
                    dependencies: {
                        a1: {
                            path: "../foo/a1",
                        },
                    },
                };
            "#,
        )
        .await;

    context
        .write_toml(
            "foo/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./a1".parse()?, "./a2".parse()?, "./a3".parse()?],
            },
        )
        .await;

    let foo_a1_project_dir = context.mkdir("foo/a1").await;
    context
        .write_file(
            "foo/a1/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        a2: "*",
                    },
                };
            "#,
        )
        .await;

    let foo_a2_project_dir = context.mkdir("foo/a2").await;
    context
        .write_file(
            "foo/a2/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        a3: "*",
                    },
                };
            "#,
        )
        .await;

    let foo_a3_project_dir = context.mkdir("foo/a3").await;
    context
        .write_file(
            "foo/a3/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        a1: "*",
                        b: {
                            path: "../../bar/b",
                        },
                    },
                };
            "#,
        )
        .await;

    context
        .write_toml(
            "bar/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec![
                    "./b".parse()?,
                    "./c1".parse()?,
                    "./c2".parse()?,
                    "./c3".parse()?,
                    "./d".parse()?,
                    "./e1".parse()?,
                    "./e2".parse()?,
                    "./f".parse()?,
                ],
            },
        )
        .await;

    let bar_b_project_dir = context.mkdir("bar/b").await;
    context
        .write_file(
            "bar/b/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c1: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_c1_project_dir = context.mkdir("bar/c1").await;
    context
        .write_file(
            "bar/c1/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c2: "*",
                        c3: "*",
                        d: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_c2_project_dir = context.mkdir("bar/c2").await;
    context
        .write_file(
            "bar/c2/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c1: "*",
                        c3: {
                            path: "../c3",
                        },
                        d: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_c3_project_dir = context.mkdir("bar/c3").await;
    context
        .write_file(
            "bar/c3/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c1: {
                            path: "../c1",
                        },
                        c2: "*",
                        d: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_d_project_dir = context.mkdir("bar/d").await;
    context
        .write_file(
            "bar/d/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        e1: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_e1_project_dir = context.mkdir("bar/e1").await;
    context
        .write_file(
            "bar/e1/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        e2: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_e2_project_dir = context.mkdir("bar/e2").await;
    context
        .write_file(
            "bar/e2/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        e1: "*",
                        f: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_f_project_dir = context.mkdir("bar/f").await;
    context
        .write_file(
            "bar/f/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        g: {
                            path: "../../baz/g",
                        },
                    },
                };
            "#,
        )
        .await;

    context
        .write_toml(
            "baz/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./g".parse()?, "./h".parse()?],
            },
        )
        .await;

    let baz_g_project_dir = context.mkdir("baz/g").await;
    context
        .write_file(
            "baz/g/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        h: "*",
                    },
                };
            "#,
        )
        .await;

    let baz_h_project_dir = context.mkdir("baz/h").await;
    context
        .write_file(
            "baz/h/project.bri",
            r"
                // Empty project
            ",
        )
        .await;

    let (projects, main_project_hash) =
        brioche_test_support::load_project(&brioche, &main_project_dir).await?;
    let foo_a1_project_hash = projects
        .find_containing_project(&foo_a1_project_dir)?
        .unwrap();
    let foo_a2_project_hash = projects
        .find_containing_project(&foo_a2_project_dir)?
        .unwrap();
    let foo_a3_project_hash = projects
        .find_containing_project(&foo_a3_project_dir)?
        .unwrap();
    let bar_b_project_hash = projects
        .find_containing_project(&bar_b_project_dir)?
        .unwrap();
    let bar_c1_project_hash = projects
        .find_containing_project(&bar_c1_project_dir)?
        .unwrap();
    let bar_c2_project_hash = projects
        .find_containing_project(&bar_c2_project_dir)?
        .unwrap();
    let bar_c3_project_hash = projects
        .find_containing_project(&bar_c3_project_dir)?
        .unwrap();
    let bar_d_project_hash = projects
        .find_containing_project(&bar_d_project_dir)?
        .unwrap();
    let bar_e1_project_hash = projects
        .find_containing_project(&bar_e1_project_dir)?
        .unwrap();
    let bar_e2_project_hash = projects
        .find_containing_project(&bar_e2_project_dir)?
        .unwrap();
    let bar_f_project_hash = projects
        .find_containing_project(&bar_f_project_dir)?
        .unwrap();
    let baz_g_project_hash = projects
        .find_containing_project(&baz_g_project_dir)?
        .unwrap();
    let baz_h_project_hash = projects
        .find_containing_project(&baz_h_project_dir)?
        .unwrap();

    let main_project_entry = projects.project_entry(main_project_hash).unwrap();
    let foo_a1_project_entry = projects.project_entry(foo_a1_project_hash).unwrap();
    let foo_a2_project_entry = projects.project_entry(foo_a2_project_hash).unwrap();
    let foo_a3_project_entry = projects.project_entry(foo_a3_project_hash).unwrap();
    let bar_b_project_entry = projects.project_entry(bar_b_project_hash).unwrap();
    let bar_c1_project_entry = projects.project_entry(bar_c1_project_hash).unwrap();
    let bar_c2_project_entry = projects.project_entry(bar_c2_project_hash).unwrap();
    let bar_c3_project_entry = projects.project_entry(bar_c3_project_hash).unwrap();
    let bar_d_project_entry = projects.project_entry(bar_d_project_hash).unwrap();
    let bar_e1_project_entry = projects.project_entry(bar_e1_project_hash).unwrap();
    let bar_e2_project_entry = projects.project_entry(bar_e2_project_hash).unwrap();
    let bar_f_project_entry = projects.project_entry(bar_f_project_hash).unwrap();
    let baz_g_project_entry = projects.project_entry(baz_g_project_hash).unwrap();
    let baz_h_project_entry = projects.project_entry(baz_h_project_hash).unwrap();

    assert_matches!(main_project_entry, ProjectEntry::Project(_));

    // a1, a2, and a3 are all part of a cycle within the workspace
    let &ProjectEntry::WorkspaceMember {
        workspace: foo_workspace,
        ..
    } = &foo_a1_project_entry
    else {
        panic!("expected foo_a_project_entry to be a WorkspaceMember");
    };
    assert_eq!(
        foo_a1_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a1".to_string().into()
        },
    );
    assert_eq!(
        foo_a2_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a2".to_string().into()
        },
    );
    assert_eq!(
        foo_a3_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a3".to_string().into()
        },
    );

    // b, d, and f aren't part of any cycles; c1, c2, and c3 are part of a
    // cycle and form one group; and e1 and e2 are part of a different cycle
    // and form a separate group (resulting in the workspace being split)
    let &ProjectEntry::WorkspaceMember {
        workspace: bar_c_workspace,
        ..
    } = &bar_c1_project_entry
    else {
        panic!("expected bar_c1_project_entry to be a WorkspaceMember");
    };
    let &ProjectEntry::WorkspaceMember {
        workspace: bar_e_workspace,
        ..
    } = &bar_e1_project_entry
    else {
        panic!("expected bar_c1_project_entry to be a WorkspaceMember");
    };
    assert_ne!(
        bar_c_workspace, bar_e_workspace,
        "expected bar_c1_project_entry and bar_e1_project_entry to be split into separate workspaces"
    );
    assert_matches!(bar_b_project_entry, ProjectEntry::Project(_));
    assert_eq!(
        bar_c1_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c1".to_string().into()
        },
    );
    assert_eq!(
        bar_c2_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c2".to_string().into()
        },
    );
    assert_eq!(
        bar_c3_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c3".to_string().into()
        },
    );
    assert_matches!(bar_d_project_entry, ProjectEntry::Project(_));
    assert_eq!(
        bar_e1_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e1".to_string().into()
        },
    );
    assert_eq!(
        bar_e2_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e2".to_string().into()
        },
    );
    assert_matches!(bar_f_project_entry, ProjectEntry::Project(_));

    // g and h are not part of a cycle
    assert_matches!(baz_g_project_entry, ProjectEntry::Project(_));
    assert_matches!(baz_h_project_entry, ProjectEntry::Project(_));

    // Ensure that loading bar/e1 as the root returns the same hash
    {
        let (brioche, _context) = brioche_test_support::brioche_test().await;
        let (projects, fresh_bar_e1_project_hash) =
            brioche_test_support::load_project(&brioche, &bar_e1_project_dir).await?;
        let fresh_bar_e2_project_hash = projects
            .find_containing_project(&bar_e2_project_dir)?
            .unwrap();

        assert_eq!(bar_e1_project_hash, fresh_bar_e1_project_hash);
        assert_eq!(bar_e2_project_hash, fresh_bar_e2_project_hash);
    }

    // Ensure that loading bar/e2 as the root returns the same hash
    {
        let (brioche, _context) = brioche_test_support::brioche_test().await;
        let (projects, fresh_bar_e2_project_hash) =
            brioche_test_support::load_project(&brioche, &bar_e2_project_dir).await?;
        let fresh_bar_e1_project_hash = projects
            .find_containing_project(&bar_e1_project_dir)?
            .unwrap();

        assert_eq!(bar_e1_project_hash, fresh_bar_e1_project_hash);
        assert_eq!(bar_e2_project_hash, fresh_bar_e2_project_hash);
    }

    Ok(())
}

#[tokio::test]
async fn test_project_load_cyclic_complex_remote() -> anyhow::Result<()> {
    let cache = brioche_test_support::new_cache();
    let (brioche, mut context) = brioche_test_with_cache(cache.clone(), false).await;

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
    // baz/h    -> baz/g

    let main_project_dir = context.mkdir("main").await;
    context
        .write_file(
            "main/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        a1: {
                            path: "../foo/a1",
                        },
                    },
                };
            "#,
        )
        .await;

    context
        .write_toml(
            "foo/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./a1".parse()?, "./a2".parse()?, "./a3".parse()?],
            },
        )
        .await;

    let foo_a1_project_dir = context.mkdir("foo/a1").await;
    context
        .write_file(
            "foo/a1/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        a2: "*",
                    },
                };
            "#,
        )
        .await;

    let foo_a2_project_dir = context.mkdir("foo/a2").await;
    context
        .write_file(
            "foo/a2/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        a3: "*",
                    },
                };
            "#,
        )
        .await;

    let foo_a3_project_dir = context.mkdir("foo/a3").await;
    context
        .write_file(
            "foo/a3/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        a1: "*",
                        b: {
                            path: "../../bar/b",
                        },
                    },
                };
            "#,
        )
        .await;

    context
        .write_toml(
            "bar/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec![
                    "./b".parse()?,
                    "./c1".parse()?,
                    "./c2".parse()?,
                    "./c3".parse()?,
                    "./d".parse()?,
                    "./e1".parse()?,
                    "./e2".parse()?,
                    "./f".parse()?,
                ],
            },
        )
        .await;

    let bar_b_project_dir = context.mkdir("bar/b").await;
    context
        .write_file(
            "bar/b/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c1: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_c1_project_dir = context.mkdir("bar/c1").await;
    context
        .write_file(
            "bar/c1/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c2: "*",
                        c3: "*",
                        d: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_c2_project_dir = context.mkdir("bar/c2").await;
    context
        .write_file(
            "bar/c2/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c1: "*",
                        c3: {
                            path: "../c3",
                        },
                        d: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_c3_project_dir = context.mkdir("bar/c3").await;
    context
        .write_file(
            "bar/c3/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        c1: {
                            path: "../c1",
                        },
                        c2: "*",
                        d: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_d_project_dir = context.mkdir("bar/d").await;
    context
        .write_file(
            "bar/d/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        e1: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_e1_project_dir = context.mkdir("bar/e1").await;
    context
        .write_file(
            "bar/e1/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        e2: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_e2_project_dir = context.mkdir("bar/e2").await;
    context
        .write_file(
            "bar/e2/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        e1: "*",
                        f: "*",
                    },
                };
            "#,
        )
        .await;

    let bar_f_project_dir = context.mkdir("bar/f").await;
    context
        .write_file(
            "bar/f/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        g1: "*",
                    },
                };
            "#,
        )
        .await;

    let baz_g1_project_hash = context
        .cached_registry_project_by_path(&cache, async |context| {
            context
                .write_toml(
                    "baz/brioche_workspace.toml",
                    &brioche_core::project::WorkspaceDefinition {
                        members: vec!["./g1".parse().unwrap(), "./g2".parse().unwrap()],
                    },
                )
                .await;

            let baz_g1_project_dir = context.mkdir("baz/g1").await;
            context
                .write_file(
                    "baz/g1/project.bri",
                    r#"
                    export const project = {
                        dependencies: {
                            g2: "*",
                        },
                    };
                "#,
                )
                .await;

            context
                .write_file(
                    "baz/g2/project.bri",
                    r#"
                        export const project = {
                            dependencies: {
                                g1: "*",
                            },
                        };
                    "#,
                )
                .await;

            baz_g1_project_dir
        })
        .await;
    context
        .mock_registry_publish_tag("g1", "latest", baz_g1_project_hash)
        .create_async()
        .await;

    let (projects, main_project_hash) =
        brioche_test_support::load_project(&brioche, &main_project_dir).await?;

    let foo_a1_project_hash = projects
        .find_containing_project(&foo_a1_project_dir)?
        .unwrap();
    let foo_a2_project_hash = projects
        .find_containing_project(&foo_a2_project_dir)?
        .unwrap();
    let foo_a3_project_hash = projects
        .find_containing_project(&foo_a3_project_dir)?
        .unwrap();
    let bar_b_project_hash = projects
        .find_containing_project(&bar_b_project_dir)?
        .unwrap();
    let bar_c1_project_hash = projects
        .find_containing_project(&bar_c1_project_dir)?
        .unwrap();
    let bar_c2_project_hash = projects
        .find_containing_project(&bar_c2_project_dir)?
        .unwrap();
    let bar_c3_project_hash = projects
        .find_containing_project(&bar_c3_project_dir)?
        .unwrap();
    let bar_d_project_hash = projects
        .find_containing_project(&bar_d_project_dir)?
        .unwrap();
    let bar_e1_project_hash = projects
        .find_containing_project(&bar_e1_project_dir)?
        .unwrap();
    let bar_e2_project_hash = projects
        .find_containing_project(&bar_e2_project_dir)?
        .unwrap();
    let bar_f_project_hash = projects
        .find_containing_project(&bar_f_project_dir)?
        .unwrap();

    let baz_g2_project_hash = projects.project_dependencies(baz_g1_project_hash).unwrap()["g2"];

    let main_project_entry = projects.project_entry(main_project_hash).unwrap();
    let foo_a1_project_entry = projects.project_entry(foo_a1_project_hash).unwrap();
    let foo_a2_project_entry = projects.project_entry(foo_a2_project_hash).unwrap();
    let foo_a3_project_entry = projects.project_entry(foo_a3_project_hash).unwrap();
    let bar_b_project_entry = projects.project_entry(bar_b_project_hash).unwrap();
    let bar_c1_project_entry = projects.project_entry(bar_c1_project_hash).unwrap();
    let bar_c2_project_entry = projects.project_entry(bar_c2_project_hash).unwrap();
    let bar_c3_project_entry = projects.project_entry(bar_c3_project_hash).unwrap();
    let bar_d_project_entry = projects.project_entry(bar_d_project_hash).unwrap();
    let bar_e1_project_entry = projects.project_entry(bar_e1_project_hash).unwrap();
    let bar_e2_project_entry = projects.project_entry(bar_e2_project_hash).unwrap();
    let bar_f_project_entry = projects.project_entry(bar_f_project_hash).unwrap();
    let baz_g1_project_entry = projects.project_entry(baz_g1_project_hash).unwrap();
    let baz_g2_project_entry = projects.project_entry(baz_g2_project_hash).unwrap();

    assert_matches!(main_project_entry, ProjectEntry::Project(_));

    // a1, a2, and a3 are all part of a cycle within the workspace
    let &ProjectEntry::WorkspaceMember {
        workspace: foo_workspace,
        ..
    } = &foo_a1_project_entry
    else {
        panic!("expected foo_a_project_entry to be a WorkspaceMember");
    };
    assert_eq!(
        foo_a1_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a1".to_string().into()
        },
    );
    assert_eq!(
        foo_a2_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a2".to_string().into()
        },
    );
    assert_eq!(
        foo_a3_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a3".to_string().into()
        },
    );

    // b, d, and f aren't part of any cycles; c1, c2, and c3 are part of a
    // cycle and form one group; and e1 and e2 are part of a different cycle
    // and form a separate group (resulting in the workspace being split)
    let &ProjectEntry::WorkspaceMember {
        workspace: bar_c_workspace,
        ..
    } = &bar_c1_project_entry
    else {
        panic!("expected bar_c1_project_entry to be a WorkspaceMember");
    };
    let &ProjectEntry::WorkspaceMember {
        workspace: bar_e_workspace,
        ..
    } = &bar_e1_project_entry
    else {
        panic!("expected bar_c1_project_entry to be a WorkspaceMember");
    };
    assert_ne!(
        bar_c_workspace, bar_e_workspace,
        "expected bar_c1_project_entry and bar_e1_project_entry to be split into separate workspaces"
    );
    assert_matches!(bar_b_project_entry, ProjectEntry::Project(_));
    assert_eq!(
        bar_c1_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c1".to_string().into()
        },
    );
    assert_eq!(
        bar_c2_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c2".to_string().into()
        },
    );
    assert_eq!(
        bar_c3_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c3".to_string().into()
        },
    );
    assert_matches!(bar_d_project_entry, ProjectEntry::Project(_));
    assert_eq!(
        bar_e1_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e1".to_string().into()
        },
    );
    assert_eq!(
        bar_e2_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e2".to_string().into()
        },
    );
    assert_matches!(bar_f_project_entry, ProjectEntry::Project(_));

    // g and h are part of a cycle
    let &ProjectEntry::WorkspaceMember {
        workspace: baz_g_workspace,
        ..
    } = &baz_g1_project_entry
    else {
        panic!("expected bar_c1_project_entry to be a WorkspaceMember");
    };
    assert_eq!(
        baz_g1_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: baz_g_workspace,
            path: "g1".to_string().into()
        },
    );
    assert_eq!(
        baz_g2_project_entry,
        ProjectEntry::WorkspaceMember {
            workspace: baz_g_workspace,
            path: "g2".to_string().into()
        },
    );

    Ok(())
}

async fn brioche_test_with_cache(
    cache: Arc<dyn object_store::ObjectStore>,
    writable: bool,
) -> (Brioche, TestContext) {
    brioche_test_support::brioche_test_with(|builder| {
        builder.cache_client(brioche_core::cache::CacheClient {
            store: Some(cache),
            writable,
            ..Default::default()
        })
    })
    .await
}
