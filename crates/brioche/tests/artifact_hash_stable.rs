#![allow(clippy::vec_init_then_push)]

use std::collections::BTreeMap;

use brioche::{
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
        "d227def794e3645dde5eaec99c66b593b6f3c2bcc0769104c3fb69c2058ae186",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, false)
            .hash()
            .to_string(),
        "d227def794e3645dde5eaec99c66b593b6f3c2bcc0769104c3fb69c2058ae186",
    ));

    asserts.push((
        brioche_test::file(hi_blob, false).hash().to_string(),
        "97908c17864a649c59ad0e2fece3ea6769e473e4b8472fe1c010e2b307713f05",
    ));
    asserts.push((
        brioche_test::lazy_file(hi_blob, false).hash().to_string(),
        "97908c17864a649c59ad0e2fece3ea6769e473e4b8472fe1c010e2b307713f05",
    ));

    asserts.push((
        brioche_test::file(hello_blob, true).hash().to_string(),
        "3648fb32ff94b7e51853e70b220a1012bfa38e33d4e88f5dfa6d1b1b0a231a4e",
    ));
    asserts.push((
        brioche_test::lazy_file(hello_blob, true).hash().to_string(),
        "3648fb32ff94b7e51853e70b220a1012bfa38e33d4e88f5dfa6d1b1b0a231a4e",
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
        "6b8e393b935024be81b311363e87e5ec04ee0c708325f1d2caee89986ebf1e2a",
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
        "6b8e393b935024be81b311363e87e5ec04ee0c708325f1d2caee89986ebf1e2a",
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
        "a9618a15769435ea5f78a60c9090bc6dd81f28b1d41da54c34c7f0ede9424ec1",
    ));
    asserts.push((
        LazyArtifact::from(brioche_test::dir_empty())
            .hash()
            .to_string(),
        "a9618a15769435ea5f78a60c9090bc6dd81f28b1d41da54c34c7f0ede9424ec1",
    ));

    asserts.push((
        brioche_test::dir(
            &brioche,
            [("foo.txt", brioche_test::file(hello_blob, false))],
        )
        .await
        .hash()
        .to_string(),
        "b09749fe2c801b5dae89cf6a98475a9c5a4a99740cccf8b89aa0459f709d71c2",
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
        "b09749fe2c801b5dae89cf6a98475a9c5a4a99740cccf8b89aa0459f709d71c2",
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
        "3416a6ebc40bdf233c4747db9af2d227e7a368948e23dabca8f54de072f567ce",
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
        "3416a6ebc40bdf233c4747db9af2d227e7a368948e23dabca8f54de072f567ce",
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
        "2fa637c2203a9ac9e5a085a1bd97591583321df41c169e0624c8660b148a11f0",
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
        "8f7d9898a19b8b2a599c78c9e59cdf0f295b7291fd2eb13ccb34f35cae0317f6",
    ));

    asserts.push((
        LazyArtifact::Download(DownloadArtifact {
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
async fn test_artifact_hash_stable_process() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        LazyArtifact::Process(ProcessArtifact {
            command: ProcessTemplate { components: vec![] },
            args: vec![],
            env: BTreeMap::default(),
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            output_scaffold: None,
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "328da364438116a512c2a376700dfd3323702290a09c3f68899502bbf1d427d7",
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
            output_scaffold: None,
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "0da38d7d4963c1f46876afdbfe7ccb12874b4f5a4740b777d7fc45324ba18a25",
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
            output_scaffold: None,
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "60432ea784f2f154358f29a5e0cdec3d503286a75d2f6de816d2b97052a2650d",
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
            output_scaffold: None,
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "2439f1cf59e1c4c723d774ba72c7aadb86e10ea47c1b9b736993e1724a0a9556",
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
            output_scaffold: None,
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "bad411f783d9d2652a2d96400488d9d46e420b92ad47f141c0a132050825b85d",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}
