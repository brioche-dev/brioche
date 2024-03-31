use assert_matches::assert_matches;
use brioche::brioche::{
    project::Projects,
    script::specifier::{
        self, read_specifier_contents, BriocheImportSpecifier, BriocheModuleSpecifier,
    },
};

mod brioche_test;

async fn resolve(
    projects: &Projects,
    specifier: &str,
    referrer: &BriocheModuleSpecifier,
) -> anyhow::Result<BriocheModuleSpecifier> {
    let specifier: BriocheImportSpecifier = specifier.parse()?;
    let resolved = specifier::resolve(projects, &specifier, referrer)?;

    Ok(resolved)
}

#[tokio::test]
async fn test_specifier_read_runtime() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let specifier: BriocheModuleSpecifier = "briocheruntime:///dist/index.js"
        .parse()
        .expect("failed to parse specifier");
    let contents = read_specifier_contents(&brioche.vfs.snapshot().await, &specifier)?;

    assert!(!contents.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_specifier_read_project() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { hello } from "./foo.bri";
                export const project = {};
            "#,
        )
        .await;
    let foo_script = r#"export const hello = "Hello world";"#;
    let foo_path = context.write_file("myproject/foo.bri", foo_script).await;

    // Ensure the project files get loaded into the VFS
    brioche_test::load_project(&brioche, &project_dir).await?;

    let specifier = BriocheModuleSpecifier::from_path(&foo_path);
    let contents = read_specifier_contents(&brioche.vfs.snapshot().await, &specifier)?;
    let contents = std::str::from_utf8(&contents).unwrap();

    assert_eq!(contents, foo_script);

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_relative() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;
    let foo_hello_path = context
        .write_file("myproject/foo/hello.txt", "Hello world")
        .await;
    let foo_inner_test_path = context
        .write_file("myproject/foo/inner/test.txt", "Hello world")
        .await;
    let foo_test_path = context
        .write_file("myproject/foo/test.txt", "Hello also!")
        .await;
    let test_path = context.write_file("myproject/test.txt", "Hi world!").await;

    let (projects, _) = brioche_test::load_project(&brioche, &project_dir).await?;
    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let sibling_specifier = resolve(&projects, "./test.txt", &referrer).await?;
    assert_eq!(
        sibling_specifier,
        BriocheModuleSpecifier::from_path(&foo_test_path),
    );

    let inner_specifier = resolve(&projects, "./inner/test.txt", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_test_path),
    );

    let outer_specifier = resolve(&projects, "../test.txt", &referrer).await?;
    assert_eq!(
        outer_specifier,
        BriocheModuleSpecifier::from_path(&test_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_project_relative() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;
    let foo_hello_path = context
        .write_file("myproject/foo/hello.txt", "Hello world")
        .await;
    let foo_inner_test_path = context
        .write_file("myproject/foo/inner/test.txt", "Hello world")
        .await;
    let foo_test_path = context
        .write_file("myproject/foo/test.txt", "Hello also!")
        .await;
    let test_path = context.write_file("myproject/test.txt", "Hi world!").await;

    let (projects, _) = brioche_test::load_project(&brioche, &project_dir).await?;
    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let root_specifier = resolve(&projects, "/test.txt", &referrer).await?;
    assert_eq!(
        root_specifier,
        BriocheModuleSpecifier::from_path(&test_path),
    );

    let foo_specifier = resolve(&projects, "/foo/test.txt", &referrer).await?;
    assert_eq!(
        foo_specifier,
        BriocheModuleSpecifier::from_path(&foo_test_path),
    );

    let inner_specifier = resolve(&projects, "/foo/inner/test.txt", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_test_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_relative_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    let main_path = context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;
    let foo_hello_path = context
        .write_file("myproject/foo/hello.txt", "Hello world")
        .await;
    let foo_inner_main_path = context
        .write_file("myproject/foo/inner/index.bri", "")
        .await;
    let foo_main_path = context.write_file("myproject/foo/index.bri", "").await;

    let (projects, _) = brioche_test::load_project(&brioche, &project_dir).await?;
    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let sibling_specifier = resolve(&projects, "./", &referrer).await?;
    assert_eq!(
        sibling_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let sibling_bare_specifier = resolve(&projects, ".", &referrer).await?;
    assert_eq!(
        sibling_bare_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let inner_specifier = resolve(&projects, "./inner", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_main_path),
    );

    let outer_specifier = resolve(&projects, "../", &referrer).await?;
    assert_eq!(
        outer_specifier,
        BriocheModuleSpecifier::from_path(&main_path),
    );

    let outer_bare_specifier = resolve(&projects, "..", &referrer).await?;
    assert_eq!(
        outer_bare_specifier,
        BriocheModuleSpecifier::from_path(&main_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_project_relative_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    let main_path = context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;
    let foo_hello_path = context
        .write_file("myproject/foo/hello.txt", "Hello world")
        .await;
    let foo_inner_main_path = context
        .write_file("myproject/foo/inner/index.bri", "")
        .await;
    let foo_main_path = context.write_file("myproject/foo/index.bri", "").await;

    let (projects, _) = brioche_test::load_project(&brioche, &project_dir).await?;
    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let root_specifier = resolve(&projects, "/", &referrer).await?;
    assert_eq!(
        root_specifier,
        BriocheModuleSpecifier::from_path(&main_path),
    );

    let foo_specifier = resolve(&projects, "/foo/", &referrer).await?;
    assert_eq!(
        foo_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let foo_bare_specifier = resolve(&projects, "/foo", &referrer).await?;
    assert_eq!(
        foo_bare_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let inner_specifier = resolve(&projects, "/foo/inner", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_main_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_subproject() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    let root_project_dir = context.mkdir("root").await;

    context
        .write_file(
            "root/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    context
        .write_file(
            "brioche-repo/foo/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        bar: "*",
                    },
                };
            "#,
        )
        .await;

    context
        .write_file(
            "brioche-repo/bar/project.bri",
            r#"
                export const project = {
                    dependencies: {
                        baz: "*",
                    },
                };
            "#,
        )
        .await;
    let bar_file_path = context.write_file("brioche-repo/bar/file.txt", "").await;

    let baz_main_path = context
        .write_file(
            "brioche-repo/baz/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;
    let _baz_file_path = context.write_file("brioche-repo/baz/file.txt", "").await;
    let _baz_inner_file_path = context
        .write_file("brioche-repo/baz/inner/file.txt", "")
        .await;

    let (projects, _) = brioche_test::load_project(&brioche, &root_project_dir).await?;
    let referrer = BriocheModuleSpecifier::from_path(&bar_file_path);

    let baz_specifier = resolve(&projects, "baz", &referrer).await?;
    assert_eq!(
        baz_specifier,
        BriocheModuleSpecifier::from_path(&baz_main_path),
    );

    // Resolving paths under a dependency is not allowed

    let baz_file_specifier = resolve(&projects, "baz/file.txt", &referrer).await;
    assert_matches!(baz_file_specifier, Err(_));

    let baz_inner_specifier = resolve(&projects, "baz/inner/file.txt", &referrer).await;
    assert_matches!(baz_inner_specifier, Err(_));

    Ok(())
}
