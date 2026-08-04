use std::sync::Arc;

use assert_matches::assert_matches;
use brioche_core::{Brioche, project::hash::ContentAddressedProjectEntry};
use brioche_test_support::TestContext;

#[tokio::test]
async fn test_project_load_cyclic_simple_by_path() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let alpha_project_ref = brioche_test_support::load_project(&brioche, &alpha_project_dir).await;

    let issues = brioche_core::project::get_all_issues(&brioche.read().await);
    assert_matches!(&issues[..], &[]);

    let alpha_project_entry = get_project_entry(&brioche, alpha_project_ref).await;
    let ContentAddressedProjectEntry::WorkspaceMember {
        workspace: alpha_workspace_hash,
        path: alpha_member_path,
    } = alpha_project_entry
    else {
        panic!("expected alpha_project to be a WorkspaceMember entry");
    };

    let beta_project_ref =
        brioche_core::project::get_dependencies(&brioche.read().await, alpha_project_ref)["beta"];
    let beta_project_entry = get_project_entry(&brioche, beta_project_ref).await;
    let ContentAddressedProjectEntry::WorkspaceMember {
        workspace: beta_workspace_hash,
        path: beta_member_path,
    } = beta_project_entry
    else {
        panic!("expected beta_project to be a WorkspaceMember entry");
    };

    // Both projects should be in the same workspace
    assert_eq!(alpha_workspace_hash, beta_workspace_hash);

    let beta_alpha_project_ref =
        brioche_core::project::get_dependencies(&brioche.read().await, beta_project_ref)["alpha"];
    assert_eq!(alpha_project_ref, beta_alpha_project_ref);

    // Paths should be relative to the workspace root
    assert_eq!(alpha_member_path, "alpha".parse().unwrap());
    assert_eq!(beta_member_path, "beta".parse().unwrap());

    let alpha_specifier =
        brioche_core::project::get_specifier(&brioche.read().await, alpha_project_ref);
    let beta_specifier =
        brioche_core::project::get_specifier(&brioche.read().await, beta_project_ref);

    assert_eq!(
        alpha_specifier,
        brioche_test_support::project_specifier_for_path(&alpha_project_dir)
    );
    assert_eq!(
        beta_specifier,
        brioche_test_support::project_specifier_for_path(&beta_project_dir)
    );
}

#[tokio::test]
async fn test_project_load_cyclic_simple_implied() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let alpha_project_ref = brioche_test_support::load_project(&brioche, &alpha_project_dir).await;

    let issues = brioche_core::project::get_all_issues(&brioche.read().await);
    assert_matches!(&issues[..], &[]);

    let alpha_project_entry = get_project_entry(&brioche, alpha_project_ref).await;
    let ContentAddressedProjectEntry::WorkspaceMember {
        workspace: alpha_workspace_hash,
        path: alpha_member_path,
    } = alpha_project_entry
    else {
        panic!("expected alpha_project to be a WorkspaceMember entry");
    };

    let beta_project_ref =
        brioche_core::project::get_dependencies(&brioche.read().await, alpha_project_ref)["beta"];
    let beta_project_entry = get_project_entry(&brioche, beta_project_ref).await;
    let ContentAddressedProjectEntry::WorkspaceMember {
        workspace: beta_workspace_hash,
        path: beta_member_path,
    } = beta_project_entry
    else {
        panic!("expected beta_project to be a WorkspaceMember entry");
    };

    // Both projects should be in the same workspace
    assert_eq!(alpha_workspace_hash, beta_workspace_hash);

    let beta_alpha_project_ref =
        brioche_core::project::get_dependencies(&brioche.read().await, beta_project_ref)["alpha"];
    assert_eq!(alpha_project_ref, beta_alpha_project_ref);

    // Paths should be relative to the workspace root
    assert_eq!(alpha_member_path, "alpha".parse().unwrap());
    assert_eq!(beta_member_path, "beta".parse().unwrap());

    let alpha_specifier =
        brioche_core::project::get_specifier(&brioche.read().await, alpha_project_ref);
    let beta_specifier =
        brioche_core::project::get_specifier(&brioche.read().await, beta_project_ref);

    assert_eq!(
        alpha_specifier,
        brioche_test_support::project_specifier_for_path(&alpha_project_dir)
    );
    assert_eq!(
        beta_specifier,
        brioche_test_support::project_specifier_for_path(&beta_project_dir)
    );
}

#[expect(clippy::similar_names)]
#[tokio::test]
async fn test_project_load_cyclic_complex() {
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
                members: vec![
                    "./a1".parse().unwrap(),
                    "./a2".parse().unwrap(),
                    "./a3".parse().unwrap(),
                ],
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
                members: vec!["./g".parse().unwrap(), "./h".parse().unwrap()],
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

    let main_project_ref = brioche_test_support::load_project(&brioche, &main_project_dir).await;
    let foo_a1_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&foo_a1_project_dir),
    )
    .unwrap();
    let foo_a2_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&foo_a2_project_dir),
    )
    .unwrap();
    let foo_a3_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&foo_a3_project_dir),
    )
    .unwrap();
    let bar_b_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_b_project_dir),
    )
    .unwrap();
    let bar_c1_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_c1_project_dir),
    )
    .unwrap();
    let bar_c2_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_c2_project_dir),
    )
    .unwrap();
    let bar_c3_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_c3_project_dir),
    )
    .unwrap();
    let bar_d_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_d_project_dir),
    )
    .unwrap();
    let bar_e1_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_e1_project_dir),
    )
    .unwrap();
    let bar_e2_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_e2_project_dir),
    )
    .unwrap();
    let bar_f_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_f_project_dir),
    )
    .unwrap();
    let baz_g_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&baz_g_project_dir),
    )
    .unwrap();
    let baz_h_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&baz_h_project_dir),
    )
    .unwrap();

    let main_project_entry = get_project_entry(&brioche, main_project_ref).await;
    let foo_a1_project_entry = get_project_entry(&brioche, foo_a1_project_ref).await;
    let foo_a2_project_entry = get_project_entry(&brioche, foo_a2_project_ref).await;
    let foo_a3_project_entry = get_project_entry(&brioche, foo_a3_project_ref).await;
    let bar_b_project_entry = get_project_entry(&brioche, bar_b_project_ref).await;
    let bar_c1_project_entry = get_project_entry(&brioche, bar_c1_project_ref).await;
    let bar_c2_project_entry = get_project_entry(&brioche, bar_c2_project_ref).await;
    let bar_c3_project_entry = get_project_entry(&brioche, bar_c3_project_ref).await;
    let bar_d_project_entry = get_project_entry(&brioche, bar_d_project_ref).await;
    let bar_e1_project_entry = get_project_entry(&brioche, bar_e1_project_ref).await;
    let bar_e2_project_entry = get_project_entry(&brioche, bar_e2_project_ref).await;
    let bar_f_project_entry = get_project_entry(&brioche, bar_f_project_ref).await;
    let baz_g_project_entry = get_project_entry(&brioche, baz_g_project_ref).await;
    let baz_h_project_entry = get_project_entry(&brioche, baz_h_project_ref).await;

    assert_matches!(main_project_entry, ContentAddressedProjectEntry::Project(_));

    // a1, a2, and a3 are all part of a cycle within the workspace
    let &ContentAddressedProjectEntry::WorkspaceMember {
        workspace: foo_workspace,
        ..
    } = &foo_a1_project_entry
    else {
        panic!("expected foo_a_project_entry to be a WorkspaceMember");
    };
    assert_eq!(
        foo_a1_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a1".parse().unwrap(),
        },
    );
    assert_eq!(
        foo_a2_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a2".parse().unwrap(),
        },
    );
    assert_eq!(
        foo_a3_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a3".parse().unwrap(),
        },
    );

    // b, d, and f aren't part of any cycles; c1, c2, and c3 are part of a
    // cycle and form one group; and e1 and e2 are part of a different cycle
    // and form a separate group (resulting in the workspace being split)
    let &ContentAddressedProjectEntry::WorkspaceMember {
        workspace: bar_c_workspace,
        ..
    } = &bar_c1_project_entry
    else {
        panic!("expected bar_c1_project_entry to be a WorkspaceMember");
    };
    let &ContentAddressedProjectEntry::WorkspaceMember {
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
    assert_matches!(
        bar_b_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );
    assert_eq!(
        bar_c1_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c1".parse().unwrap(),
        },
    );
    assert_eq!(
        bar_c2_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c2".parse().unwrap(),
        },
    );
    assert_eq!(
        bar_c3_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c3".parse().unwrap(),
        },
    );
    assert_matches!(
        bar_d_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );
    assert_eq!(
        bar_e1_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e1".parse().unwrap(),
        },
    );
    assert_eq!(
        bar_e2_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e2".parse().unwrap(),
        },
    );
    assert_matches!(
        bar_f_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );

    // g and h are not part of a cycle
    assert_matches!(
        baz_g_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );
    assert_matches!(
        baz_h_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );

    // Ensure that loading bar/e1 as the root returns the same hash
    {
        let (brioche, _context) = brioche_test_support::brioche_test().await;
        let fresh_bar_e1_project_ref =
            brioche_test_support::load_project(&brioche, &bar_e1_project_dir).await;
        let fresh_bar_e2_project_ref = brioche_core::project::get_project_by_specifier(
            &brioche.read().await,
            &brioche_test_support::project_specifier_for_path(&bar_e2_project_dir),
        )
        .unwrap();

        let fresh_bar_e1_project_hash = brioche_core::project::hash::hash_project(
            &mut brioche.write().await,
            fresh_bar_e1_project_ref,
        )
        .await
        .unwrap();
        let fresh_bar_e2_project_hash = brioche_core::project::hash::hash_project(
            &mut brioche.write().await,
            fresh_bar_e2_project_ref,
        )
        .await
        .unwrap();

        assert_eq!(
            bar_e1_project_entry.project_hash(),
            fresh_bar_e1_project_hash
        );
        assert_eq!(
            bar_e2_project_entry.project_hash(),
            fresh_bar_e2_project_hash
        );
    }

    // Ensure that loading bar/e2 as the root returns the same hash
    {
        let (brioche, _context) = brioche_test_support::brioche_test().await;
        let fresh_bar_e2_project_ref =
            brioche_test_support::load_project(&brioche, &bar_e2_project_dir).await;
        let fresh_bar_e1_project_ref = brioche_core::project::get_project_by_specifier(
            &brioche.read().await,
            &brioche_test_support::project_specifier_for_path(&bar_e1_project_dir),
        )
        .unwrap();

        let fresh_bar_e1_project_hash = brioche_core::project::hash::hash_project(
            &mut brioche.write().await,
            fresh_bar_e1_project_ref,
        )
        .await
        .unwrap();
        let fresh_bar_e2_project_hash = brioche_core::project::hash::hash_project(
            &mut brioche.write().await,
            fresh_bar_e2_project_ref,
        )
        .await
        .unwrap();

        assert_eq!(
            bar_e1_project_entry.project_hash(),
            fresh_bar_e1_project_hash
        );
        assert_eq!(
            bar_e2_project_entry.project_hash(),
            fresh_bar_e2_project_hash
        );
    }
}

#[expect(clippy::similar_names)]
#[tokio::test]
async fn test_project_load_cyclic_complex_remote() {
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
                members: vec![
                    "./a1".parse().unwrap(),
                    "./a2".parse().unwrap(),
                    "./a3".parse().unwrap(),
                ],
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

    let main_project_ref = brioche_test_support::load_project(&brioche, &main_project_dir).await;
    let foo_a1_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&foo_a1_project_dir),
    )
    .unwrap();
    let foo_a2_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&foo_a2_project_dir),
    )
    .unwrap();
    let foo_a3_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&foo_a3_project_dir),
    )
    .unwrap();
    let bar_b_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_b_project_dir),
    )
    .unwrap();
    let bar_c1_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_c1_project_dir),
    )
    .unwrap();
    let bar_c2_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_c2_project_dir),
    )
    .unwrap();
    let bar_c3_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_c3_project_dir),
    )
    .unwrap();
    let bar_d_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_d_project_dir),
    )
    .unwrap();
    let bar_e1_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_e1_project_dir),
    )
    .unwrap();
    let bar_e2_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_e2_project_dir),
    )
    .unwrap();
    let bar_f_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_test_support::project_specifier_for_path(&bar_f_project_dir),
    )
    .unwrap();
    let baz_g1_project_ref = brioche_core::project::get_project_by_specifier(
        &brioche.read().await,
        &brioche_core::project::ProjectSpecifier::Hash(baz_g1_project_hash),
    )
    .unwrap();
    let baz_g2_project_ref =
        brioche_core::project::get_dependencies(&brioche.read().await, baz_g1_project_ref)["g2"];

    let main_project_entry = get_project_entry(&brioche, main_project_ref).await;
    let foo_a1_project_entry = get_project_entry(&brioche, foo_a1_project_ref).await;
    let foo_a2_project_entry = get_project_entry(&brioche, foo_a2_project_ref).await;
    let foo_a3_project_entry = get_project_entry(&brioche, foo_a3_project_ref).await;
    let bar_b_project_entry = get_project_entry(&brioche, bar_b_project_ref).await;
    let bar_c1_project_entry = get_project_entry(&brioche, bar_c1_project_ref).await;
    let bar_c2_project_entry = get_project_entry(&brioche, bar_c2_project_ref).await;
    let bar_c3_project_entry = get_project_entry(&brioche, bar_c3_project_ref).await;
    let bar_d_project_entry = get_project_entry(&brioche, bar_d_project_ref).await;
    let bar_e1_project_entry = get_project_entry(&brioche, bar_e1_project_ref).await;
    let bar_e2_project_entry = get_project_entry(&brioche, bar_e2_project_ref).await;
    let bar_f_project_entry = get_project_entry(&brioche, bar_f_project_ref).await;
    let baz_g1_project_entry = get_project_entry(&brioche, baz_g1_project_ref).await;
    let baz_g2_project_entry = get_project_entry(&brioche, baz_g2_project_ref).await;

    assert_matches!(main_project_entry, ContentAddressedProjectEntry::Project(_));

    // a1, a2, and a3 are all part of a cycle within the workspace
    let &ContentAddressedProjectEntry::WorkspaceMember {
        workspace: foo_workspace,
        ..
    } = &foo_a1_project_entry
    else {
        panic!("expected foo_a_project_entry to be a WorkspaceMember");
    };
    assert_eq!(
        foo_a1_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a1".parse().unwrap(),
        },
    );
    assert_eq!(
        foo_a2_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a2".parse().unwrap(),
        },
    );
    assert_eq!(
        foo_a3_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: foo_workspace,
            path: "a3".parse().unwrap(),
        },
    );

    // b, d, and f aren't part of any cycles; c1, c2, and c3 are part of a
    // cycle and form one group; and e1 and e2 are part of a different cycle
    // and form a separate group (resulting in the workspace being split)
    let &ContentAddressedProjectEntry::WorkspaceMember {
        workspace: bar_c_workspace,
        ..
    } = &bar_c1_project_entry
    else {
        panic!("expected bar_c1_project_entry to be a WorkspaceMember");
    };
    let &ContentAddressedProjectEntry::WorkspaceMember {
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
    assert_matches!(
        bar_b_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );
    assert_eq!(
        bar_c1_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c1".parse().unwrap(),
        },
    );
    assert_eq!(
        bar_c2_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c2".parse().unwrap(),
        },
    );
    assert_eq!(
        bar_c3_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_c_workspace,
            path: "c3".parse().unwrap(),
        },
    );
    assert_matches!(
        bar_d_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );
    assert_eq!(
        bar_e1_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e1".parse().unwrap(),
        },
    );
    assert_eq!(
        bar_e2_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: bar_e_workspace,
            path: "e2".parse().unwrap(),
        },
    );
    assert_matches!(
        bar_f_project_entry,
        ContentAddressedProjectEntry::Project(_)
    );

    // g1 and g2 are part of a cycle
    let &ContentAddressedProjectEntry::WorkspaceMember {
        workspace: baz_g_workspace,
        ..
    } = &baz_g1_project_entry
    else {
        panic!("expected baz_g1_project_entry to be a WorkspaceMember");
    };
    assert_eq!(
        baz_g1_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: baz_g_workspace,
            path: "g1".parse().unwrap(),
        },
    );
    assert_eq!(
        baz_g2_project_entry,
        ContentAddressedProjectEntry::WorkspaceMember {
            workspace: baz_g_workspace,
            path: "g2".parse().unwrap(),
        },
    );
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

async fn get_project_entry(
    brioche: &Brioche,
    project_ref: brioche_core::project::ProjectRef,
) -> ContentAddressedProjectEntry {
    brioche_core::project::hash::get_content_addressed_project_entries(
        &mut brioche.write().await,
        project_ref,
    )
    .await
    .remove(&project_ref)
    .unwrap()
}
