#![allow(clippy::vec_init_then_push)]

use std::collections::BTreeMap;

use brioche::brioche::{
    artifact::{
        DownloadArtifact, LazyArtifact, ProcessArtifact, ProcessTemplate, ProcessTemplateComponent,
    },
    platform::Platform,
    Hash,
};
use pretty_assertions::assert_eq;

mod brioche_test;

#[tokio::test]
async fn test_artifact_hash_stable_file() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;
    let hi_blob = brioche_test::blob(&brioche, b"hi").await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::file(hello_blob, false).hash().to_string(),
        "a6b7080d0b8c52c608680ba9c86cbaf8ddb2fa68782ca56a5e6bb640add8220f",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, false)
            .hash()
            .to_string(),
        "2b49864e5a614c122da7aca0503c47df6410608b0874e4730e9229b3faa908b7",
    ));

    asserts.push((
        brioche_test::file(hi_blob, false).hash().to_string(),
        "a325a432160ad49a11697bd2f18157b5124ed48376453bc851f627b8cef58eac",
    ));
    asserts.push((
        brioche_test::lazy_file(hi_blob, false).hash().to_string(),
        "81a8bfd7a02285a6eae9fab40ffaccd649711e21cf0bca5f192f52062192b86f",
    ));

    asserts.push((
        brioche_test::file(hello_blob, true).hash().to_string(),
        "20ed0a264da3b8fb5c27fca2d70778590a764129de7157070aa25fbcf31ac7ea",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, true).hash().to_string(),
        "b176eb8dea170f247a2eafe1a4e35e1598818ce077e13b7783b56c3c9db7af91",
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
        "47bb5f180d0206135e843e3aa81f12106124851e8f48f0143e7d6a35053c6f6e",
    ));
    asserts.push((
        brioche_test::lazy_file_with_resources(
            hello_blob,
            false,
            LazyArtifact::from(
                brioche_test::dir(
                    &brioche,
                    [("foo.txt", brioche_test::file(hello_blob, false))],
                )
                .await,
            ),
        )
        .hash()
        .to_string(),
        "3ded790822fdddc49a48fef5ec466d4cfa5b36629799e732be561dad7cad1cef",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_artifact_hash_stable_directory() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;
    let hi_blob = brioche_test::blob(&brioche, b"hi").await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::dir_empty().hash().to_string(),
        "2f51f105073a1b8b7644b23c3992439a3f82e6b54c0d06b10ed65575a6283135",
    ));
    asserts.push((
        LazyArtifact::from(brioche_test::dir_empty())
            .hash()
            .to_string(),
        "2f51f105073a1b8b7644b23c3992439a3f82e6b54c0d06b10ed65575a6283135",
    ));

    asserts.push((
        brioche_test::dir(
            &brioche,
            [("foo.txt", brioche_test::file(hello_blob, false))],
        )
        .await
        .hash()
        .to_string(),
        "773e23ae9256d18dbea707c79f6d2d2c018b910a992b6c75e71ef26dd7a32233",
    ));
    asserts.push((
        LazyArtifact::from(
            brioche_test::dir(
                &brioche,
                [("foo.txt", brioche_test::file(hello_blob, false))],
            )
            .await,
        )
        .hash()
        .to_string(),
        "773e23ae9256d18dbea707c79f6d2d2c018b910a992b6c75e71ef26dd7a32233",
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
        "04155278894583935b64b9fe655e6d865ac3aff93fa790bcff4bbe9710ebe08b",
    ));
    asserts.push((
        LazyArtifact::from(
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
        "04155278894583935b64b9fe655e6d865ac3aff93fa790bcff4bbe9710ebe08b",
    ));

    asserts.push((
        brioche_test::lazy_dir([(
            "foo",
            LazyArtifact::Merge {
                directories: vec![
                    brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                    brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                ],
            },
        )])
        .hash()
        .to_string(),
        "59c5921cc57e0ee5f7b2206cdb64c1f874b8043d1aca6d93d5d079740704879a",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_artifact_hash_stable_symlink() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::lazy_symlink(b"foo").hash().to_string(),
        "5beab9f7b650a3804bf79f30355309421e61a3978087a79d1baa299ddc8fac7c",
    ));
    asserts.push((
        brioche_test::symlink(b"foo").hash().to_string(),
        "5beab9f7b650a3804bf79f30355309421e61a3978087a79d1baa299ddc8fac7c",
    ));

    asserts.push((
        brioche_test::lazy_symlink(b"/foo").hash().to_string(),
        "f815995c6f75a4a37a05b2ce2c9c8605a1f6c194f6a4671bc4cf76ace03e2860",
    ));
    asserts.push((
        brioche_test::symlink(b"/foo").hash().to_string(),
        "f815995c6f75a4a37a05b2ce2c9c8605a1f6c194f6a4671bc4cf76ace03e2860",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_artifact_hash_stable_download() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        LazyArtifact::Download(DownloadArtifact {
            url: "https://example.com/foo".parse()?,
            hash: Hash::Sha256 { value: vec![0x00] },
        })
        .hash()
        .to_string(),
        "9a6643fe132824d8cfb0a2949a7d0e6ae0b84cc73cb7bdf833dd0ab1d65b0937",
    ));

    asserts.push((
        LazyArtifact::Download(DownloadArtifact {
            url: "https://example.com/foo".parse()?,
            hash: Hash::Sha256 { value: vec![0x01] },
        })
        .hash()
        .to_string(),
        "0abe03ac1f459602aec0f3ddacbe025d7e318541290287c6357aa284446279b1",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_artifact_hash_stable_process() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        LazyArtifact::Process(ProcessArtifact {
            command: ProcessTemplate { components: vec![] },
            args: vec![],
            env: BTreeMap::default(),
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "bcd2e9a337eed8ffa23c200779874689a47b99e695c2e9b78fbf40f341b67ea8",
    ));

    asserts.push((
        LazyArtifact::Process(ProcessArtifact {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![],
            env: BTreeMap::default(),
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "6fd9e60413f06f081ca8fb4848fc64bd43b56bacbc5754b8b4a7b8998c93c5ed",
    ));

    asserts.push((
        LazyArtifact::Process(ProcessArtifact {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "/usr/bin/env".into(),
                }],
            },
            args: vec![ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal { value: "sh".into() }],
            }],
            env: BTreeMap::default(),
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "5d79fba94bcfa6a7f059c2d7f672a0a76d00aaf60ab41fd3871b2ce48f8889c3",
    ));

    asserts.push((
        LazyArtifact::Process(ProcessArtifact {
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
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "7504517f95f4aa3de6af11f7cc5e6b9eba2f67037c2df425756c09a6b87bc98a",
    ));

    asserts.push((
        LazyArtifact::Process(ProcessArtifact {
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
                            artifact: brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                        },
                        ProcessTemplateComponent::Literal {
                            value: "/bin".into(),
                        },
                    ],
                },
            )]),
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "bfda06bcc63fb6fb88cb2d3be3a8bd3ba6a549c22c7b6f2be0009d311d8f398b",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}
