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
        "bf8f872243626c7c0f504f238692b0ef5d24fc0fe1ab0158f1ba98791f510f06",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, false)
            .hash()
            .to_string(),
        "f4d1681d0ef3bb2283c7353849580fc2557972e2f77ae5c62a7aa7b0bd55e419",
    ));

    asserts.push((
        brioche_test::file(hi_blob, false).hash().to_string(),
        "92e0405a0e9d24c11ee17d7b83de03be05b798d42db078aa6be4a1df55a27d28",
    ));
    asserts.push((
        brioche_test::lazy_file(hi_blob, false).hash().to_string(),
        "937cef462f78e7050dcf9b8f0e94064bc5d82872ad6b14373a750d8811ec38da",
    ));

    asserts.push((
        brioche_test::file(hello_blob, true).hash().to_string(),
        "567dcb7dd0f4e646db07611e0a2a4b7e98cab91ea3f1ba6b6285a09cfee9687f",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, true).hash().to_string(),
        "c60ae6cd5b1c5ec18cf9b452c08ca912527786e76460fdc5e4e794b1e4093f7c",
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
        "19573f5e0dda1d7f794d24d29706ad01cfa1b66b12d832b46b0fc1c3f9a496b2",
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
        "403aec4f2551b4092d2c225972ac568d4d2dbb941b7c7f35e615dec968a83de9",
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
        "2a370e3e0f3bfd8421d72a594a93a947fb4c133c480e3a898f6f65102a415302",
    ));
    asserts.push((
        LazyArtifact::from(brioche_test::dir_empty())
            .hash()
            .to_string(),
        "2a370e3e0f3bfd8421d72a594a93a947fb4c133c480e3a898f6f65102a415302",
    ));

    asserts.push((
        brioche_test::dir(
            &brioche,
            [("foo.txt", brioche_test::file(hello_blob, false))],
        )
        .await
        .hash()
        .to_string(),
        "2f4c5a79a756b60c20de6849a7d2b86cc5b4ea0535e7a90647bd9b6d6797eb75",
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
        "2f4c5a79a756b60c20de6849a7d2b86cc5b4ea0535e7a90647bd9b6d6797eb75",
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
        "1b971999cc4b2686e865b3c5184eacecbc5bba10649092d47d7fc23c729f7ce4",
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
        "1b971999cc4b2686e865b3c5184eacecbc5bba10649092d47d7fc23c729f7ce4",
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
        "9847f1a30047cda80890cb81762c56d7000699039b9c0ac54317d6a628fa5f73",
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
        "7d2cc695d170b1f0f842c09f083336f646bcd44d329a100b949a7baec28c0755",
    ));
    asserts.push((
        brioche_test::symlink(b"foo").hash().to_string(),
        "7d2cc695d170b1f0f842c09f083336f646bcd44d329a100b949a7baec28c0755",
    ));

    asserts.push((
        brioche_test::lazy_symlink(b"/foo").hash().to_string(),
        "c8c223c202dff16e2c257dd1f5e764c2ab919bb6ae4e019e1c6eee1aa66a6393",
    ));
    asserts.push((
        brioche_test::symlink(b"/foo").hash().to_string(),
        "c8c223c202dff16e2c257dd1f5e764c2ab919bb6ae4e019e1c6eee1aa66a6393",
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
        "a5a538836efdfbe31cdffe8ba2838f3e03bb872a870a9eb52dec979e5410897e",
    ));

    asserts.push((
        LazyArtifact::Download(DownloadArtifact {
            url: "https://example.com/foo".parse()?,
            hash: Hash::Sha256 { value: vec![0x01] },
        })
        .hash()
        .to_string(),
        "747cd20070228a01dabeb531be9d8cc68c02d8c12ac4bf572d4a16234c3b8758",
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
        "ae9c054dbaef92aa11c733dc46990b1b52eb539378a074517d0032a014722b3e",
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
        "66900de43c1d7425a3613aed799e3728bc87e004843c019e56f619c52ef903cd",
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
        "636ada2393c79fb91afb11b3a536daa6bd6f92e66be3ebe2e521efe7644da4c1",
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
        "bd926c17a17897022fdae3d6e899eeb770b0994caf34366c0b6ed0acc8e2648f",
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
        "2fa878536d82f6be721ae00b8b404f3aef20b67d301fa9535ad867ca4ea4bf0c",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}
