use brioche_core::{
    recipe::Recipe,
    script::runtime::{JsRuntime, initialize_js_platform},
};

#[tokio::test]
async fn test_script_eval_basic() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export default function () {
                    return {
                        briocheSerialize() {
                            return {
                                type: "create_file",
                                content: "hello world",
                                executable: false,
                            };
                        }
                    };
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project(&mut *brioche.write().await, &project_dir).await;

    let js_runtime = JsRuntime::new(&brioche, initialize_js_platform())
        .await
        .unwrap();
    let default_ref = js_runtime
        .get_recipe_export(project_ref, "default")
        .await
        .unwrap();
    let default = brioche
        .read()
        .await
        .recipes()
        .get_recipe(default_ref)
        .clone();

    assert_eq!(
        *default,
        Recipe::CreateFile {
            content: b"hello world".into(),
            executable: false,
            resources: None
        }
    );
}
