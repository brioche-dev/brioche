#[tokio::test]
async fn test_project_hash_stable_simple() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                export const project = {};
            ",
        )
        .await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;
    let project_hash = brioche_core::projects::hash::hash_project(&brioche, project_ref)
        .await
        .unwrap();

    assert_eq!(
        project_hash.to_string(),
        "e9d088c8cef7d6620f313684bb4804b71a4b9dd2e1e128273ce572ef18c4e09d",
    );

    Ok(())
}

#[tokio::test]
async fn test_project_hash_stable_simple_no_definition() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r"").await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;
    let project_hash = brioche_core::projects::hash::hash_project(&brioche, project_ref)
        .await
        .unwrap();

    assert_eq!(
        project_hash.to_string(),
        "0c5d6dcbd231292f3bc02e07154c52bd2b162ec61dd82d1ff2af08ba7e3821bf",
    );

    Ok(())
}

#[tokio::test]
async fn test_project_hash_stable_with_path_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let main_project_dir = context.mkdir("mainproject").await;
    context
        .write_file(
            "mainproject/project.bri",
            r#"
                import "depproject";
                export const project = {
                    dependencies: {
                        depproject: {
                            path: "../depproject",
                        },
                    },
                };
            "#,
        )
        .await;

    context
        .write_file(
            "depproject/project.bri",
            r"
                export const project = {};
            ",
        )
        .await;

    let project_ref = brioche_test_support::load_project(&brioche, &main_project_dir).await;
    let project_hash = brioche_core::projects::hash::hash_project(&brioche, project_ref)
        .await
        .unwrap();
    let dep_project_ref =
        brioche_core::projects::get_dependencies(&brioche, project_ref).await["depproject"];
    let dep_project_hash = brioche_core::projects::hash::hash_project(&brioche, dep_project_ref)
        .await
        .unwrap();

    assert_eq!(
        dep_project_hash.to_string(),
        "e9d088c8cef7d6620f313684bb4804b71a4b9dd2e1e128273ce572ef18c4e09d"
    );
    assert_eq!(
        project_hash.to_string(),
        "7f39fd30711614961b812cbdecf4d4a1d36862b83b7202b407c26a14b0911ff8"
    );

    Ok(())
}
