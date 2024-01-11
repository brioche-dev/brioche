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
        brioche_test::lazy_file(hello_blob, false)
            .hash()
            .to_string(),
        "a6b7080d0b8c52c608680ba9c86cbaf8ddb2fa68782ca56a5e6bb640add8220f",
    ));
    asserts.push((
        brioche_test::file(hello_blob, false).hash().to_string(),
        "a6b7080d0b8c52c608680ba9c86cbaf8ddb2fa68782ca56a5e6bb640add8220f",
    ));

    asserts.push((
        brioche_test::lazy_file(hi_blob, false).hash().to_string(),
        "a325a432160ad49a11697bd2f18157b5124ed48376453bc851f627b8cef58eac",
    ));
    asserts.push((
        brioche_test::file(hi_blob, false).hash().to_string(),
        "a325a432160ad49a11697bd2f18157b5124ed48376453bc851f627b8cef58eac",
    ));

    asserts.push((
        brioche_test::lazy_file(hello_blob, true).hash().to_string(),
        "20ed0a264da3b8fb5c27fca2d70778590a764129de7157070aa25fbcf31ac7ea",
    ));
    asserts.push((
        brioche_test::file(hello_blob, true).hash().to_string(),
        "20ed0a264da3b8fb5c27fca2d70778590a764129de7157070aa25fbcf31ac7ea",
    ));

    asserts.push((
        brioche_test::lazy_file_with_resources(
            hello_blob,
            false,
            brioche_test::lazy_dir_value([("foo.txt", brioche_test::lazy_file(hello_blob, false))]),
        )
        .hash()
        .to_string(),
        "27c41efc8d056e2d571f781032416d95714cd77da82afc40135ce21c0453f4a6",
    ));
    asserts.push((
        brioche_test::file_with_resources(
            hello_blob,
            false,
            brioche_test::dir_value([("foo.txt", brioche_test::file(hello_blob, false))]),
        )
        .hash()
        .to_string(),
        "27c41efc8d056e2d571f781032416d95714cd77da82afc40135ce21c0453f4a6",
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
        brioche_test::lazy_dir_empty().hash().to_string(),
        "2f51f105073a1b8b7644b23c3992439a3f82e6b54c0d06b10ed65575a6283135",
    ));
    asserts.push((
        brioche_test::dir_empty().hash().to_string(),
        "2f51f105073a1b8b7644b23c3992439a3f82e6b54c0d06b10ed65575a6283135",
    ));

    asserts.push((
        brioche_test::lazy_dir([("foo.txt", brioche_test::lazy_file(hello_blob, false))])
            .hash()
            .to_string(),
        "939a1c5b18900279593cea01e1a78e6ea0604eebd080f845011353c476b64a2c",
    ));
    asserts.push((
        brioche_test::dir([("foo.txt", brioche_test::file(hello_blob, false))])
            .hash()
            .to_string(),
        "939a1c5b18900279593cea01e1a78e6ea0604eebd080f845011353c476b64a2c",
    ));

    asserts.push((
        brioche_test::lazy_dir([
            ("foo.txt", brioche_test::lazy_file(hello_blob, false)),
            (
                "bar",
                brioche_test::lazy_dir([("hi.txt", brioche_test::lazy_file(hi_blob, false))]),
            ),
        ])
        .hash()
        .to_string(),
        "9a381be2fd8619d55273d037c1ceb6068ddb3fdfaffff85cec0c9fdd78a7bf93",
    ));
    asserts.push((
        brioche_test::dir([
            ("foo.txt", brioche_test::file(hello_blob, false)),
            (
                "bar",
                brioche_test::dir([("hi.txt", brioche_test::file(hi_blob, false))]),
            ),
        ])
        .hash()
        .to_string(),
        "9a381be2fd8619d55273d037c1ceb6068ddb3fdfaffff85cec0c9fdd78a7bf93",
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
        "e68087284af6d54595c2235c97630ac5d2fcdeba87f27e91af278b6c3ab576c4",
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
        "d9df4f961f526c9124fb19b2a9e9696a83f1d2cdd3f390660e78c465b93f79a8",
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
        "8eaec0e082b66e6344d0ad10e493f4f7d0ca0f47515a2011a5c851d1eaf3419b",
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
        "d94023b82816a1b8cb316cfe175a1ab8dcd9c0029cd35065a098474910f9cdab",
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
        "caf4553cb77d7527cd01d0dc954bf8d083d1f313bdffde2496e9859c1b1f38c1",
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
        "b065cc3f66e108550f62cbbf6202ac2dd9ca423b01ee7ca6d7352782e675601f",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}
