use brioche_core::script::{evaluate::evaluate, initialize_js_platform};

#[tokio::test]
async fn test_script_ops_version() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let recipe = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;
    let artifact = brioche_test_support::bake_without_meta(&brioche, recipe).await?;

    let version_blob = brioche_test_support::blob(&brioche, brioche_core::VERSION).await;
    assert_eq!(artifact, brioche_test_support::file(version_blob, false));

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestStackFrame {
    file_name: String,
    line_number: i32,
}

#[tokio::test]
async fn test_script_osp_stack_frames_from_exception() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { foo } from "./foo.bri";

                export default () => {
                    return {
                        briocheSerialize: () => {
                            return {
                                type: "create_file",
                                content: JSON.stringify(foo()),
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
    context
        .write_file(
            "myproject/foo.bri",
            r#"
                import { bar } from "./bar.bri";
                export function foo(): unknown {
                    return bar();
                }
            "#,
        )
        .await;
    context
        .write_file(
            "myproject/bar.bri",
            r#"
                import { getStackFrames } from "./stack_frames.bri";
                export function bar(): unknown {
                    return getStackFrames();
                }
            "#,
        )
        .await;
    context
        .write_file(
            "myproject/stack_frames.bri",
            r#"
                export function getStackFrames(): unknown {
                    const error = new Error();
                    const frames = (globalThis as any).Deno.core.ops.op_brioche_stack_frames_from_exception(error);

                    // Get just the filename and line number
                    return frames.map((frame: any) => {
                        return {
                            fileName: frame.fileName.split("/").at(-1),
                            lineNumber: frame.lineNumber,
                        };
                    });
                    return frames;
                }
            "#,
        )
        .await;

    let (projects, project_hash) =
        brioche_test_support::load_project(&brioche, &project_dir).await?;

    let recipe = evaluate(
        &brioche,
        initialize_js_platform(),
        &projects,
        project_hash,
        "default",
    )
    .await?
    .value;
    let artifact = brioche_test_support::bake_without_meta(&brioche, recipe).await?;

    let brioche_core::recipe::Artifact::File(file) = artifact else {
        panic!("expected file artifact");
    };
    let blob_path = brioche_core::blob::local_blob_path(&brioche, file.content_blob);
    let blob_content = tokio::fs::read_to_string(&blob_path).await?;

    let stack_frames = serde_json::from_str::<Vec<TestStackFrame>>(&blob_content)?;

    assert_eq!(
        stack_frames,
        vec![
            TestStackFrame {
                file_name: "stack_frames.bri".to_string(),
                line_number: 3,
            },
            TestStackFrame {
                file_name: "bar.bri".to_string(),
                line_number: 4,
            },
            TestStackFrame {
                file_name: "foo.bri".to_string(),
                line_number: 4,
            },
            TestStackFrame {
                file_name: "project.bri".to_string(),
                line_number: 9,
            },
        ]
    );

    Ok(())
}
