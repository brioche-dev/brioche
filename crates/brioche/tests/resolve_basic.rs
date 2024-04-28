use brioche::{recipe::Recipe, Brioche};

mod brioche_test;

pub async fn resolve_to_recipe(brioche: &Brioche, recipe: &Recipe) -> Recipe {
    let resolved = brioche_test::resolve_without_meta(brioche, recipe.clone()).await;
    Recipe::from(resolved.expect("failed to resolve"))
}

#[tokio::test]
async fn test_resolve_basic() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let blob_hello = brioche_test::blob(&brioche, b"hello").await;
    let file_hello = brioche_test::lazy_file(blob_hello, false);

    let lazy_empty_dir = brioche_test::lazy_dir_empty();
    let empty_dir = brioche_test::dir_empty();

    let lazy_hello_dir = brioche_test::lazy_dir([
        ("hello.txt", file_hello.clone()),
        ("hi.txt", file_hello.clone()),
    ]);
    let hello_dir = brioche_test::dir(
        &brioche,
        [
            ("hello.txt", brioche_test::file(blob_hello, false)),
            ("hi.txt", brioche_test::file(blob_hello, false)),
        ],
    )
    .await;

    let lazy_complex_dir = brioche_test::lazy_dir([
        ("hello.txt", file_hello.clone()),
        ("hello", lazy_hello_dir.clone()),
        ("empty", lazy_empty_dir.clone()),
        ("link", brioche_test::lazy_symlink("hello.txt")),
    ]);
    let complex_dir = brioche_test::dir(
        &brioche,
        [
            ("hello.txt", brioche_test::file(blob_hello, false)),
            (
                "hello",
                brioche_test::dir(
                    &brioche,
                    [
                        ("hello.txt", brioche_test::file(blob_hello, false)),
                        ("hi.txt", brioche_test::file(blob_hello, false)),
                    ],
                )
                .await,
            ),
            ("empty", empty_dir.clone()),
            ("link", brioche_test::symlink("hello.txt")),
        ],
    )
    .await;

    assert_eq!(resolve_to_recipe(&brioche, &file_hello).await, file_hello);

    assert_eq!(
        resolve_to_recipe(&brioche, &lazy_empty_dir).await,
        empty_dir.into()
    );

    assert_eq!(
        resolve_to_recipe(&brioche, &lazy_hello_dir).await,
        hello_dir.into()
    );

    assert_eq!(
        resolve_to_recipe(&brioche, &lazy_complex_dir).await,
        complex_dir.into()
    );

    Ok(())
}
