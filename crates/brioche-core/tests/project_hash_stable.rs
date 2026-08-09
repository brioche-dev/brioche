#[tokio::test]
async fn test_project_hash_stable_simple() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            indoc::indoc! {r"
                export const project = {};
            "},
        )
        .await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;
    let project_hash =
        brioche_core::project::hash::hash_project(&mut *brioche.write().await, project_ref)
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

    let project_dir = context.mkdir("myproject").await;
    context.write_file("myproject/project.bri", r"").await;

    let project_ref = brioche_test_support::load_project(&brioche, &project_dir).await;
    let project_hash =
        brioche_core::project::hash::hash_project(&mut *brioche.write().await, project_ref)
            .await
            .unwrap();

    assert_eq!(
        project_hash.to_string(),
        "0c5d6dcbd231292f3bc02e07154c52bd2b162ec61dd82d1ff2af08ba7e3821bf",
    );
}

#[tokio::test]
async fn test_project_hash_stable_with_path_dep() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let project_ref = brioche_test_support::load_project(&brioche, &main_project_dir).await;
    let project_hash =
        brioche_core::project::hash::hash_project(&mut *brioche.write().await, project_ref)
            .await
            .unwrap();
    let dep_project_ref =
        brioche_core::project::get_dependencies(&*brioche.read().await, project_ref)["depproject"];
    let dep_project_hash =
        brioche_core::project::hash::hash_project(&mut *brioche.write().await, dep_project_ref)
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
