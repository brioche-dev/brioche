use brioche_core::{recipe::Recipe, Brioche};

pub async fn bake_to_recipe(brioche: &Brioche, recipe: &Recipe) -> Recipe {
    let artifact = brioche_test_support::bake_without_meta(brioche, recipe.clone()).await;
    Recipe::from(artifact.expect("failed to bake"))
}

#[tokio::test]
async fn test_bake_basic() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let blob_hello = brioche_test_support::blob(&brioche, b"hello").await;
    let file_hello = brioche_test_support::lazy_file(blob_hello, false);

    let lazy_empty_dir = brioche_test_support::lazy_dir_empty();
    let empty_dir = brioche_test_support::dir_empty();

    let lazy_hello_dir = brioche_test_support::lazy_dir([
        ("hello.txt", file_hello.clone()),
        ("hi.txt", file_hello.clone()),
    ]);
    let hello_dir = brioche_test_support::dir(
        &brioche,
        [
            ("hello.txt", brioche_test_support::file(blob_hello, false)),
            ("hi.txt", brioche_test_support::file(blob_hello, false)),
        ],
    )
    .await;

    let lazy_complex_dir = brioche_test_support::lazy_dir([
        ("hello.txt", file_hello.clone()),
        ("hello", lazy_hello_dir.clone()),
        ("empty", lazy_empty_dir.clone()),
        ("link", brioche_test_support::lazy_symlink("hello.txt")),
    ]);
    let complex_dir = brioche_test_support::dir(
        &brioche,
        [
            ("hello.txt", brioche_test_support::file(blob_hello, false)),
            (
                "hello",
                brioche_test_support::dir(
                    &brioche,
                    [
                        ("hello.txt", brioche_test_support::file(blob_hello, false)),
                        ("hi.txt", brioche_test_support::file(blob_hello, false)),
                    ],
                )
                .await,
            ),
            ("empty", empty_dir.clone()),
            ("link", brioche_test_support::symlink("hello.txt")),
        ],
    )
    .await;

    assert_eq!(bake_to_recipe(&brioche, &file_hello).await, file_hello);

    assert_eq!(
        bake_to_recipe(&brioche, &lazy_empty_dir).await,
        empty_dir.into()
    );

    assert_eq!(
        bake_to_recipe(&brioche, &lazy_hello_dir).await,
        hello_dir.into()
    );

    assert_eq!(
        bake_to_recipe(&brioche, &lazy_complex_dir).await,
        complex_dir.into()
    );

    Ok(())
}
