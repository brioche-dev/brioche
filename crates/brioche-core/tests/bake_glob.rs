use core::panic;

use brioche_core::{
    Brioche,
    recipe::{Directory, Recipe, WithMeta},
};

pub async fn bake_to_recipe(brioche: &Brioche, recipe: &Recipe) -> Recipe {
    let artifact = brioche_test_support::bake_without_meta(brioche, recipe.clone()).await;
    Recipe::from(artifact.expect("failed to bake"))
}

pub async fn bake_glob(brioche: &Brioche, recipe: &Recipe, patterns: &[&str]) -> Directory {
    let glob_recipe = Recipe::Glob {
        directory: Box::new(WithMeta::without_meta(recipe.clone())),
        patterns: patterns.iter().map(|&pattern| pattern.into()).collect(),
    };

    let Recipe::Directory(result) = bake_to_recipe(brioche, &glob_recipe).await else {
        panic!("expected baked glob to return a directory");
    };

    result
}

#[tokio::test]
async fn test_bake_glob_basic() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let blob = brioche_test_support::blob(&brioche, b"hello").await;

    let directory = brioche_test_support::lazy_dir([(
        "foo",
        brioche_test_support::lazy_dir([
            (
                "bar",
                brioche_test_support::lazy_dir([
                    (
                        "baz",
                        brioche_test_support::lazy_dir([(
                            "hello.include",
                            brioche_test_support::lazy_file(blob, false),
                        )]),
                    ),
                    ("hi.include", brioche_test_support::lazy_file(blob, false)),
                    ("other.txt", brioche_test_support::lazy_file(blob, false)),
                ]),
            ),
            (
                "included",
                brioche_test_support::lazy_dir([(
                    "other.txt",
                    brioche_test_support::lazy_file(blob, false),
                )]),
            ),
            (
                "something.txt",
                brioche_test_support::lazy_file(blob, false),
            ),
        ]),
    )]);

    let globbed = bake_glob(
        &brioche,
        &directory,
        &["**/*.include", "foo/included", "foo/something.txt"],
    )
    .await;

    assert_eq!(
        globbed,
        brioche_test_support::dir_value(
            &brioche,
            [(
                "foo",
                brioche_test_support::dir(
                    &brioche,
                    [
                        (
                            "bar",
                            brioche_test_support::dir(
                                &brioche,
                                [
                                    (
                                        "baz",
                                        brioche_test_support::dir(
                                            &brioche,
                                            [(
                                                "hello.include",
                                                brioche_test_support::file(blob, false)
                                            ),]
                                        )
                                        .await
                                    ),
                                    ("hi.include", brioche_test_support::file(blob, false)),
                                ]
                            )
                            .await
                        ),
                        (
                            "included",
                            brioche_test_support::dir(
                                &brioche,
                                [("other.txt", brioche_test_support::file(blob, false)),]
                            )
                            .await
                        ),
                        ("something.txt", brioche_test_support::file(blob, false)),
                    ]
                )
                .await
            ),]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_glob_non_utf8() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let blob = brioche_test_support::blob(&brioche, b"hello").await;

    let directory = brioche_test_support::lazy_dir([
        (b"foo\x80.txt", brioche_test_support::lazy_file(blob, false)),
        (b"bar\x81.txt", brioche_test_support::lazy_file(blob, false)),
        (b"bar\x82.txt", brioche_test_support::lazy_file(blob, false)),
        (b"baz\x83.txt", brioche_test_support::lazy_file(blob, false)),
    ]);

    let globbed = bake_glob(
        &brioche,
        &directory,
        &[
            // Wildcards should match invalid UTF-8 sequences
            "foo*.txt",
            // The Unicode replacement character should match any byte
            // in an invalid UTF-8 sequences
            "bar�.txt",
        ],
    )
    .await;

    assert_eq!(
        globbed,
        brioche_test_support::dir_value(
            &brioche,
            [
                (b"foo\x80.txt", brioche_test_support::file(blob, false)),
                (b"bar\x81.txt", brioche_test_support::file(blob, false)),
                (b"bar\x82.txt", brioche_test_support::file(blob, false)),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_glob_no_matches() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let blob = brioche_test_support::blob(&brioche, b"hello").await;

    let directory = brioche_test_support::lazy_dir([(
        "foo",
        brioche_test_support::lazy_dir([
            (
                "bar",
                brioche_test_support::lazy_dir([
                    (
                        "baz",
                        brioche_test_support::lazy_dir([(
                            "hello.include",
                            brioche_test_support::lazy_file(blob, false),
                        )]),
                    ),
                    ("hi.include", brioche_test_support::lazy_file(blob, false)),
                    ("other.txt", brioche_test_support::lazy_file(blob, false)),
                ]),
            ),
            (
                "included",
                brioche_test_support::lazy_dir([(
                    "other.txt",
                    brioche_test_support::lazy_file(blob, false),
                )]),
            ),
            (
                "something.txt",
                brioche_test_support::lazy_file(blob, false),
            ),
        ]),
    )]);

    let globbed = bake_glob(&brioche, &directory, &["**/*.nothing"]).await;

    assert_eq!(globbed, brioche_test_support::empty_dir_value());

    Ok(())
}

#[tokio::test]
async fn test_bake_glob_no_patterns() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test_support::brioche_test().await;

    let blob = brioche_test_support::blob(&brioche, b"hello").await;

    let directory = brioche_test_support::lazy_dir([(
        "foo",
        brioche_test_support::lazy_dir([
            (
                "bar",
                brioche_test_support::lazy_dir([
                    (
                        "baz",
                        brioche_test_support::lazy_dir([(
                            "hello.include",
                            brioche_test_support::lazy_file(blob, false),
                        )]),
                    ),
                    ("hi.include", brioche_test_support::lazy_file(blob, false)),
                    ("other.txt", brioche_test_support::lazy_file(blob, false)),
                ]),
            ),
            (
                "included",
                brioche_test_support::lazy_dir([(
                    "other.txt",
                    brioche_test_support::lazy_file(blob, false),
                )]),
            ),
            (
                "something.txt",
                brioche_test_support::lazy_file(blob, false),
            ),
        ]),
    )]);

    let globbed = bake_glob(&brioche, &directory, &[]).await;

    assert_eq!(globbed, brioche_test_support::empty_dir_value());

    Ok(())
}
