#![allow(clippy::vec_init_then_push)]

use std::collections::BTreeMap;

use brioche::{
    platform::Platform,
    recipe::{DownloadRecipe, ProcessRecipe, ProcessTemplate, ProcessTemplateComponent, Recipe},
    Hash,
};
use pretty_assertions::assert_eq;

mod brioche_test;

#[tokio::test]
async fn test_recipe_hash_stable_file() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;
    let hi_blob = brioche_test::blob(&brioche, b"hi").await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::file(hello_blob, false).hash().to_string(),
        "042cbcf2fc68cccbde296bcb6bae718ca625866b1c245c8e980cef7a0a5cf269",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, false)
            .hash()
            .to_string(),
        "042cbcf2fc68cccbde296bcb6bae718ca625866b1c245c8e980cef7a0a5cf269",
    ));

    asserts.push((
        brioche_test::file(hi_blob, false).hash().to_string(),
        "0db68b5a219eb5ec35887d86401a2991445acd5b37e3a5893bf78ffce0df346b",
    ));
    asserts.push((
        brioche_test::lazy_file(hi_blob, false).hash().to_string(),
        "0db68b5a219eb5ec35887d86401a2991445acd5b37e3a5893bf78ffce0df346b",
    ));

    asserts.push((
        brioche_test::file(hello_blob, true).hash().to_string(),
        "65b1f6376f34e88546d5f313b8e2dd7f2516e5c2d78500c7f6f474cc0141fd20",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, true).hash().to_string(),
        "65b1f6376f34e88546d5f313b8e2dd7f2516e5c2d78500c7f6f474cc0141fd20",
    ));

    asserts.push((
        brioche_test::file_with_resources(
            hello_blob,
            false,
            brioche_test::dir_value(
                &brioche,
                [("foo.txt", brioche_test::file(hello_blob, false))],
            )
            .await,
        )
        .hash()
        .to_string(),
        "b02bb2a50344129ab3684eaf4eafedc8a550049e85006ee6a7fdefd79c6db345",
    ));
    asserts.push((
        brioche_test::lazy_file_with_resources(
            hello_blob,
            false,
            Recipe::from(
                brioche_test::dir(
                    &brioche,
                    [("foo.txt", brioche_test::file(hello_blob, false))],
                )
                .await,
            ),
        )
        .hash()
        .to_string(),
        "b02bb2a50344129ab3684eaf4eafedc8a550049e85006ee6a7fdefd79c6db345",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_recipe_hash_stable_directory() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;
    let hi_blob = brioche_test::blob(&brioche, b"hi").await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::dir_empty().hash().to_string(),
        "2e4799f8f3b8e26761b4ab8700222d0b112737b601870de583f371f8ba03d08d",
    ));
    asserts.push((
        Recipe::from(brioche_test::dir_empty()).hash().to_string(),
        "2e4799f8f3b8e26761b4ab8700222d0b112737b601870de583f371f8ba03d08d",
    ));

    asserts.push((
        brioche_test::dir(
            &brioche,
            [("foo.txt", brioche_test::file(hello_blob, false))],
        )
        .await
        .hash()
        .to_string(),
        "39beed924e377831f4fed7dbc9a7c5cdb700d8d677a27c873190a81eef69ca28",
    ));
    asserts.push((
        Recipe::from(
            brioche_test::dir(
                &brioche,
                [("foo.txt", brioche_test::file(hello_blob, false))],
            )
            .await,
        )
        .hash()
        .to_string(),
        "39beed924e377831f4fed7dbc9a7c5cdb700d8d677a27c873190a81eef69ca28",
    ));

    asserts.push((
        brioche_test::dir(
            &brioche,
            [
                ("foo.txt", brioche_test::file(hello_blob, false)),
                (
                    "bar",
                    brioche_test::dir(&brioche, [("hi.txt", brioche_test::file(hi_blob, false))])
                        .await,
                ),
            ],
        )
        .await
        .hash()
        .to_string(),
        "b49eb0f9321dbcb1a6b0345a8b430a76e4f63b85047d21474c8eed8601e7ccc8",
    ));
    asserts.push((
        Recipe::from(
            brioche_test::dir(
                &brioche,
                [
                    ("foo.txt", brioche_test::file(hello_blob, false)),
                    (
                        "bar",
                        brioche_test::dir(
                            &brioche,
                            [("hi.txt", brioche_test::file(hi_blob, false))],
                        )
                        .await,
                    ),
                ],
            )
            .await,
        )
        .hash()
        .to_string(),
        "b49eb0f9321dbcb1a6b0345a8b430a76e4f63b85047d21474c8eed8601e7ccc8",
    ));

    asserts.push((
        brioche_test::lazy_dir([(
            "foo",
            Recipe::Merge {
                directories: vec![
                    brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                    brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                ],
            },
        )])
        .hash()
        .to_string(),
        "2fa637c2203a9ac9e5a085a1bd97591583321df41c169e0624c8660b148a11f0",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_recipe_hash_stable_symlink() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::lazy_symlink(b"foo").hash().to_string(),
        "148b4e771e39cd0309404ac40bb0ce382557dea7ac258be23eb633cf051b4446",
    ));
    asserts.push((
        brioche_test::symlink(b"foo").hash().to_string(),
        "148b4e771e39cd0309404ac40bb0ce382557dea7ac258be23eb633cf051b4446",
    ));

    asserts.push((
        brioche_test::lazy_symlink(b"/foo").hash().to_string(),
        "9819e3b1d518885c9e759a59799b46ab27d39a4d15c6b891807b192bdc5c2225",
    ));
    asserts.push((
        brioche_test::symlink(b"/foo").hash().to_string(),
        "9819e3b1d518885c9e759a59799b46ab27d39a4d15c6b891807b192bdc5c2225",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_recipe_hash_stable_download() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        Recipe::Download(DownloadRecipe {
            url: "https://example.com/foo".parse()?,
            hash: Hash::Sha256 { value: vec![0x00] },
        })
        .hash()
        .to_string(),
        "8f7d9898a19b8b2a599c78c9e59cdf0f295b7291fd2eb13ccb34f35cae0317f6",
    ));

    asserts.push((
        Recipe::Download(DownloadRecipe {
            url: "https://example.com/foo".parse()?,
            hash: Hash::Sha256 { value: vec![0x01] },
        })
        .hash()
        .to_string(),
        "746f52c35bc39e72adb69f3e2daa4bceef0ea568b48c2cee23e100576b6acc92",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_recipe_hash_stable_process() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        Recipe::Process(ProcessRecipe {
            command: ProcessTemplate { components: vec![] },
            args: vec![],
            env: BTreeMap::default(),
            dependencies: vec![],
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
            is_unsafe: false,
            networking: false,
        })
        .hash()
        .to_string(),
        "328da364438116a512c2a376700dfd3323702290a09c3f68899502bbf1d427d7",
    ));

    asserts.push((
        Recipe::Process(ProcessRecipe {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![],
            env: BTreeMap::default(),
            dependencies: vec![],
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
            is_unsafe: false,
            networking: false,
        })
        .hash()
        .to_string(),
        "0da38d7d4963c1f46876afdbfe7ccb12874b4f5a4740b777d7fc45324ba18a25",
    ));

    asserts.push((
        Recipe::Process(ProcessRecipe {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal { value: "sh".into() }],
            }],
            env: BTreeMap::default(),
            dependencies: vec![],
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
            is_unsafe: false,
            networking: false,
        })
        .hash()
        .to_string(),
        "60432ea784f2f154358f29a5e0cdec3d503286a75d2f6de816d2b97052a2650d",
    ));

    asserts.push((
        Recipe::Process(ProcessRecipe {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal { value: "sh".into() }],
            }],
            env: BTreeMap::from_iter([(
                "PATH".into(),
                ProcessTemplate {
                    components: vec![ProcessTemplateComponent::Literal {
                        value: "/bin".into(),
                    }],
                },
            )]),
            dependencies: vec![],
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
            is_unsafe: false,
            networking: false,
        })
        .hash()
        .to_string(),
        "2439f1cf59e1c4c723d774ba72c7aadb86e10ea47c1b9b736993e1724a0a9556",
    ));

    asserts.push((
        Recipe::Process(ProcessRecipe {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal { value: "sh".into() }],
            }],
            env: BTreeMap::from_iter([(
                "PATH".into(),
                ProcessTemplate {
                    components: vec![
                        ProcessTemplateComponent::Input {
                            recipe: brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                        },
                        ProcessTemplateComponent::Literal {
                            value: "/bin".into(),
                        },
                    ],
                },
            )]),
            dependencies: vec![],
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
            is_unsafe: false,
            networking: false,
        })
        .hash()
        .to_string(),
        "9cf41f944bfa3d76830e6edf7de386d67f7f56a3cd737aa3632550ef1edee9d2",
    ));

    asserts.push((
        Recipe::Process(ProcessRecipe {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal { value: "sh".into() }],
            }],
            env: BTreeMap::from_iter([(
                "PATH".into(),
                ProcessTemplate {
                    components: vec![
                        ProcessTemplateComponent::Input {
                            recipe: brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                        },
                        ProcessTemplateComponent::Literal {
                            value: "/bin".into(),
                        },
                    ],
                },
            )]),
            dependencies: vec![],
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
            is_unsafe: true,
            networking: false,
        })
        .hash()
        .to_string(),
        "2badfafffeb1deadd0e384816ff202800c48080a7fbc1aaff354d0e6515337f2",
    ));

    asserts.push((
        Recipe::Process(ProcessRecipe {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal { value: "sh".into() }],
            }],
            env: BTreeMap::from_iter([(
                "PATH".into(),
                ProcessTemplate {
                    components: vec![
                        ProcessTemplateComponent::Input {
                            recipe: brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                        },
                        ProcessTemplateComponent::Literal {
                            value: "/bin".into(),
                        },
                    ],
                },
            )]),
            dependencies: vec![],
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
            is_unsafe: true,
            networking: true,
        })
        .hash()
        .to_string(),
        "5d14c32fce7134257ede8918680a83d9b33a292879192ac99461202851ab82e5",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}
