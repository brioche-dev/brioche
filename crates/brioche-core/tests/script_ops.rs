use brioche_core::script::evaluate::evaluate;

mod brioche_test;

#[tokio::test]
async fn test_script_ops_version() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                const BRIOCHE_VERSION = (globalThis as any).Deno.core.ops.op_brioche_version();
                export default () => {
                    return {
                        briocheSerialize: () => {
                            return {
                                type: "create_file",
                                content: BRIOCHE_VERSION,
                                executable: false,
                                resources: {
                                    type: "directory",
                                    entries: {},
                                },
                            };
                        },
                    };
                };
            "#,
        )
        .await;

    let (projects, project_hash) = brioche_test::load_project(&brioche, &project_dir).await?;

    let recipe = evaluate(&brioche, &projects, project_hash, "default")
        .await?
        .value;
    let artifact = brioche_test::bake_without_meta(&brioche, recipe).await?;

    let version_blob = brioche_test::blob(&brioche, brioche_core::VERSION).await;
    assert_eq!(artifact, brioche_test::file(version_blob, false));

    Ok(())
}
