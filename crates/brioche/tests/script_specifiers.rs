use assert_matches::assert_matches;
use brioche::brioche::{
    script::specifier::{
        self, read_specifier_contents, read_specifier_contents_sync, BriocheImportSpecifier,
        BriocheModuleSpecifier,
    },
    Brioche,
};
use tokio::io::AsyncReadExt as _;

mod brioche_test;

async fn resolve(
    brioche: &Brioche,
    specifier: &str,
    referrer: &BriocheModuleSpecifier,
) -> anyhow::Result<BriocheModuleSpecifier> {
    let specifier: BriocheImportSpecifier = specifier.parse()?;
    let resolved = specifier::resolve(brioche, &specifier, referrer).await?;

    Ok(resolved)
}

#[tokio::test]
async fn test_specifier_read_runtime_sync() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let specifier: BriocheModuleSpecifier = "briocheruntime:///dist/index.js"
        .parse()
        .expect("failed to parse specifier");
    let mut reader = read_specifier_contents_sync(&specifier)?.expect("specifier not found");
    let mut contents = String::new();
    reader.read_to_string(&mut contents)?;

    assert!(!contents.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_specifier_read_runtime() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let specifier: BriocheModuleSpecifier = "briocheruntime:///dist/index.js"
        .parse()
        .expect("failed to parse specifier");
    let mut reader = read_specifier_contents(&specifier)
        .await?
        .expect("specifier not found");
    let mut contents = String::new();
    reader.read_to_string(&mut contents).await?;

    assert!(!contents.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_specifier_read_project() -> anyhow::Result<()> {
    let (_brioche, context) = brioche_test::brioche_test().await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;
    let foo_path = context.write_file("myproject/foo.txt", "Hello world").await;

    let specifier = BriocheModuleSpecifier::from_path(&foo_path);
    let mut reader = read_specifier_contents(&specifier)
        .await?
        .expect("specifier not found");
    let mut contents = String::new();
    reader.read_to_string(&mut contents).await?;

    assert_eq!(contents, "Hello world");

    Ok(())
}

#[tokio::test]
async fn test_specifier_read_project_sync() -> anyhow::Result<()> {
    let (_brioche, context) = brioche_test::brioche_test().await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {};
            "#,
        )
        .await;
    let foo_path = context.write_file("myproject/foo.txt", "Hello world").await;

    let specifier = BriocheModuleSpecifier::from_path(&foo_path);
    let mut reader = read_specifier_contents_sync(&specifier)?.expect("specifier not found");
    let mut contents = String::new();
    reader.read_to_string(&mut contents)?;

    assert_eq!(contents, "Hello world");

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_relative() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

    context.mkdir("myproject").await;
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

    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let sibling_specifier = resolve(&brioche, "./test.txt", &referrer).await?;
    assert_eq!(
        sibling_specifier,
        BriocheModuleSpecifier::from_path(&foo_test_path),
    );

    let inner_specifier = resolve(&brioche, "./inner/test.txt", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_test_path),
    );

    let outer_specifier = resolve(&brioche, "../test.txt", &referrer).await?;
    assert_eq!(
        outer_specifier,
        BriocheModuleSpecifier::from_path(&test_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_project_relative() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let root_specifier = resolve(&brioche, "/test.txt", &referrer).await?;
    assert_eq!(
        root_specifier,
        BriocheModuleSpecifier::from_path(&test_path),
    );

    let foo_specifier = resolve(&brioche, "/foo/test.txt", &referrer).await?;
    assert_eq!(
        foo_specifier,
        BriocheModuleSpecifier::from_path(&foo_test_path),
    );

    let inner_specifier = resolve(&brioche, "/foo/inner/test.txt", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_test_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_relative_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let sibling_specifier = resolve(&brioche, "./", &referrer).await?;
    assert_eq!(
        sibling_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let sibling_bare_specifier = resolve(&brioche, ".", &referrer).await?;
    assert_eq!(
        sibling_bare_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let inner_specifier = resolve(&brioche, "./inner", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_main_path),
    );

    let outer_specifier = resolve(&brioche, "../", &referrer).await?;
    assert_eq!(
        outer_specifier,
        BriocheModuleSpecifier::from_path(&main_path),
    );

    let outer_bare_specifier = resolve(&brioche, "..", &referrer).await?;
    assert_eq!(
        outer_bare_specifier,
        BriocheModuleSpecifier::from_path(&main_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_project_relative_dir() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let referrer = BriocheModuleSpecifier::from_path(&foo_hello_path);

    let root_specifier = resolve(&brioche, "/", &referrer).await?;
    assert_eq!(
        root_specifier,
        BriocheModuleSpecifier::from_path(&main_path),
    );

    let foo_specifier = resolve(&brioche, "/foo/", &referrer).await?;
    assert_eq!(
        foo_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let foo_bare_specifier = resolve(&brioche, "/foo", &referrer).await?;
    assert_eq!(
        foo_bare_specifier,
        BriocheModuleSpecifier::from_path(&foo_main_path),
    );

    let inner_specifier = resolve(&brioche, "/foo/inner", &referrer).await?;
    assert_eq!(
        inner_specifier,
        BriocheModuleSpecifier::from_path(&foo_inner_main_path),
    );

    Ok(())
}

#[tokio::test]
async fn test_specifier_resolve_subproject() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test::brioche_test().await;

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

    let referrer = BriocheModuleSpecifier::from_path(&bar_file_path);

    let baz_specifier = resolve(&brioche, "baz", &referrer).await?;
    assert_eq!(
        baz_specifier,
        BriocheModuleSpecifier::from_path(&baz_main_path),
    );

    // Resolving paths under a dependency is not allowed

    let baz_file_specifier = resolve(&brioche, "baz/file.txt", &referrer).await;
    assert_matches!(baz_file_specifier, Err(_));

    let baz_inner_specifier = resolve(&brioche, "baz/inner/file.txt", &referrer).await;
    assert_matches!(baz_inner_specifier, Err(_));

    Ok(())
}
