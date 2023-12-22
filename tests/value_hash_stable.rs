#![allow(clippy::vec_init_then_push)]

use std::collections::BTreeMap;

use brioche::brioche::{
    platform::Platform,
    value::{DownloadValue, LazyValue, ProcessTemplate, ProcessTemplateComponent, ProcessValue},
    Hash,
};
use pretty_assertions::assert_eq;

mod brioche_test;

#[tokio::test]
async fn test_value_hash_stable_file() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;
    let hi_blob = brioche_test::blob(&brioche, b"hi").await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::lazy_file(hello_blob, false)
            .hash()
            .to_string(),
        "9a079c4980c3379e63c59ce4e2081144eb857e259b48f3851314cc38a955699f",
    ));
    asserts.push((
        brioche_test::file(hello_blob, false).hash().to_string(),
        "9a079c4980c3379e63c59ce4e2081144eb857e259b48f3851314cc38a955699f",
    ));

    asserts.push((
        brioche_test::lazy_file(hi_blob, false).hash().to_string(),
        "fa86db9cdc5939031686fc72bb07e4fa768951a8d12277abcfe2f1f20af3adab",
    ));
    asserts.push((
        brioche_test::file(hi_blob, false).hash().to_string(),
        "fa86db9cdc5939031686fc72bb07e4fa768951a8d12277abcfe2f1f20af3adab",
    ));

    asserts.push((
        brioche_test::lazy_file(hello_blob, true).hash().to_string(),
        "cdbf5f4dc5965b5e27973be505fa8aadcb51ce4918679f05ebd00c562498d332",
    ));
    asserts.push((
        brioche_test::file(hello_blob, true).hash().to_string(),
        "cdbf5f4dc5965b5e27973be505fa8aadcb51ce4918679f05ebd00c562498d332",
    ));

    asserts.push((
        brioche_test::lazy_file_with_resources(
            hello_blob,
            false,
            brioche_test::lazy_dir_value([("foo.txt", brioche_test::lazy_file(hello_blob, false))]),
        )
        .hash()
        .to_string(),
        "bdb8b2cdc7e9e229487eeaed7794a2db6aed8c212efbfd29a29c338db1866204",
    ));
    asserts.push((
        brioche_test::file_with_resources(
            hello_blob,
            false,
            brioche_test::dir_value([("foo.txt", brioche_test::file(hello_blob, false))]),
        )
        .hash()
        .to_string(),
        "bdb8b2cdc7e9e229487eeaed7794a2db6aed8c212efbfd29a29c338db1866204",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_value_hash_stable_directory() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;
    let hi_blob = brioche_test::blob(&brioche, b"hi").await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::lazy_dir_empty().hash().to_string(),
        "120d30430ef023b30a692898135888e4c874ba0b5c947c7669a52cc4ee7ad627",
    ));
    asserts.push((
        brioche_test::dir_empty().hash().to_string(),
        "120d30430ef023b30a692898135888e4c874ba0b5c947c7669a52cc4ee7ad627",
    ));

    asserts.push((
        brioche_test::lazy_dir([("foo.txt", brioche_test::lazy_file(hello_blob, false))])
            .hash()
            .to_string(),
        "c94c3463d774128e360d449855c77ae4ac445e8d68686ce4a7acd16ba449d5a2",
    ));
    asserts.push((
        brioche_test::dir([("foo.txt", brioche_test::file(hello_blob, false))])
            .hash()
            .to_string(),
        "c94c3463d774128e360d449855c77ae4ac445e8d68686ce4a7acd16ba449d5a2",
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
        "48aa91e007cb0558f1066d3515d04b84af2045934a922c35dca40d6f5d7b818c",
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
        "48aa91e007cb0558f1066d3515d04b84af2045934a922c35dca40d6f5d7b818c",
    ));

    asserts.push((
        brioche_test::lazy_dir([(
            "foo",
            LazyValue::Merge {
                directories: vec![
                    brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                    brioche_test::without_meta(brioche_test::lazy_dir_empty()),
                ],
            },
        )])
        .hash()
        .to_string(),
        "98baf3417ffedf6cdf106be3cf417948d7b238efae2c4f5d59c19b55f52436f9",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_value_hash_stable_symlink() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        brioche_test::lazy_symlink(b"foo").hash().to_string(),
        "26a52225e2c81f75ff86229de52eac402d8249457f990e2ec3765f1402f1a858",
    ));
    asserts.push((
        brioche_test::symlink(b"foo").hash().to_string(),
        "26a52225e2c81f75ff86229de52eac402d8249457f990e2ec3765f1402f1a858",
    ));

    asserts.push((
        brioche_test::lazy_symlink(b"/foo").hash().to_string(),
        "adc5ca4776f9692ce4927b5eddfc3805103e44bc40ff9052d81728a219971518",
    ));
    asserts.push((
        brioche_test::symlink(b"/foo").hash().to_string(),
        "adc5ca4776f9692ce4927b5eddfc3805103e44bc40ff9052d81728a219971518",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_value_hash_stable_download() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        LazyValue::Download(DownloadValue {
            url: "https://example.com/foo".parse()?,
            hash: Hash::Sha256 { value: vec![0x00] },
        })
        .hash()
        .to_string(),
        "bd0b77e2b00affe0e5125f225f8be8c526a5ed16d3cdc9c9216097d356ed7927",
    ));

    asserts.push((
        LazyValue::Download(DownloadValue {
            url: "https://example.com/foo".parse()?,
            hash: Hash::Sha256 { value: vec![0x01] },
        })
        .hash()
        .to_string(),
        "cbe804cc8104b653ef055d63a702c948be1e04dd8d1cb381ebe31b3b0d627404",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}

#[tokio::test]
async fn test_value_hash_stable_process() -> anyhow::Result<()> {
    let (_brioche, _context) = brioche_test::brioche_test().await;

    let mut asserts = vec![];

    asserts.push((
        LazyValue::Process(ProcessValue {
            command: ProcessTemplate { components: vec![] },
            args: vec![],
            env: BTreeMap::default(),
            work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
            platform: Platform::X86_64Linux,
        })
        .hash()
        .to_string(),
        "95d771b576c1744b51a825e301e4ea0a1461d8e4cf871f31182096a01c80afdd",
    ));

    asserts.push((
        LazyValue::Process(ProcessValue {
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
        "283cd9be9a77f7422b83aaf3e3498c526716ae9ee6cf7e2add4239c6cac23f18",
    ));

    asserts.push((
        LazyValue::Process(ProcessValue {
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
        "9a4a51b8ac5e8a64029e931dd68803e56833366bf7c8f92291eaa408f8a7155a",
    ));

    asserts.push((
        LazyValue::Process(ProcessValue {
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
        "97d57a379ac1bcf7b45bfc6cbc0948611356837e185fb9ac8edaf42d99b0caa9",
    ));

    asserts.push((
        LazyValue::Process(ProcessValue {
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
                            value: brioche_test::without_meta(brioche_test::lazy_dir_empty()),
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
        "2f43b3dd5342d3ee746994eb4406658b35e59ab92ec78c1ffbd430e0f3280d97",
    ));

    let left: Vec<_> = asserts.iter().map(|(left, _)| left).collect();
    let right: Vec<_> = asserts.iter().map(|(_, right)| right).collect();

    assert_eq!(left, right);

    Ok(())
}
