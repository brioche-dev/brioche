use brioche::brioche::vfs::Vfs;

mod brioche_test;

#[tokio::test]
async fn test_project_hash_immutable() -> anyhow::Result<()> {
    let project_hash_1 = {
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
                                    listingBlob: null,
                                }
                            },
                        };
                    };
                "#,
            )
            .await;

        let (_, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
        project_hash
    };

    let project_hash_2 = {
        let (brioche, context) = brioche_test::brioche_test().await;

        let project_dir = context.mkdir("myproject2").await;

        context
            .write_file(
                "myproject2/project.bri",
                r#"
                    export const project = {};
                    export default () => {
                        return {
                            briocheSerialize: () => {
                                return {
                                    type: "directory",
                                    listingBlob: null,
                                }
                            },
                        };
                    };
                "#,
            )
            .await;

        let (_, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
        project_hash
    };

    assert_eq!(project_hash_1, project_hash_2);

    Ok(())
}

#[tokio::test]
async fn test_project_hash_mutable() -> anyhow::Result<()> {
    let project_hash_1 = {
        let (brioche, context) = brioche_test::brioche_test_with(|b| b.vfs(Vfs::mutable())).await;

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
                                    listingBlob: null,
                                }
                            },
                        };
                    };
                "#,
            )
            .await;

        let (_, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
        project_hash
    };

    let project_hash_2 = {
        let (brioche, context) = brioche_test::brioche_test_with(|b| b.vfs(Vfs::mutable())).await;

        let project_dir = context.mkdir("myproject2").await;

        context
            .write_file(
                "myproject2/project.bri",
                r#"
                    export const project = {};
                    export default () => {
                        return {
                            briocheSerialize: () => {
                                return {
                                    type: "directory",
                                    listingBlob: null,
                                }
                            },
                        };
                    };
                "#,
            )
            .await;

        let (_, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;
        project_hash
    };

    assert_ne!(project_hash_1, project_hash_2);

    Ok(())
}
