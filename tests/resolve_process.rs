use std::collections::BTreeMap;

use assert_matches::assert_matches;
use pretty_assertions::assert_eq;

use brioche::brioche::{
    platform::current_platform,
    value::{
        ArchiveFormat, CompleteValue, CompressionFormat, DownloadValue, File, LazyValue,
        ProcessTemplate, ProcessTemplateComponent, ProcessValue, UnpackValue,
    },
    Hash,
};
use brioche_test::resolve_without_meta;

mod brioche_test;

fn tpl(s: impl AsRef<[u8]>) -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::Literal {
            value: s.as_ref().into(),
        }],
    }
}

fn output_path() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::OutputPath],
    }
}

fn home_path() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::HomeDir],
    }
}

fn resources_path() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::ResourcesDir],
    }
}

fn work_dir_path() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::WorkDir],
    }
}

fn temp_dir_path() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::TempDir],
    }
}

fn template_input(input: LazyValue) -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::Input {
            value: brioche_test::without_meta(input),
        }],
    }
}

fn tpl_join(templates: impl IntoIterator<Item = ProcessTemplate>) -> ProcessTemplate {
    ProcessTemplate {
        components: templates
            .into_iter()
            .flat_map(|template| template.components)
            .collect(),
    }
}

fn sha256_hash(hash: &str) -> Hash {
    Hash::Sha256 {
        value: hex::decode(hash).unwrap(),
    }
}

fn utils() -> LazyValue {
    let utils_download = LazyValue::Download(DownloadValue {
        url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/utils_amd64_linux.tar.zstd".parse().unwrap(),
        hash: sha256_hash("eb29ea059fcd9ca457841f5c79151721a74761a31610d694bce61a62f4de6d33"),
    });

    LazyValue::Unpack(UnpackValue {
        file: Box::new(brioche_test::without_meta(utils_download)),
        archive: ArchiveFormat::Tar,
        compression: CompressionFormat::Zstd,
    })
}

// HACK: This semaphore is used to ensure only one process test runs at a time.
// For some reason, running all these tests concurrently causes the method
// `unshare::run::Command.spawn()` to hang (this happens about 5-10% of the
// time when running `cargo test 'test_resolve_process'`). This is most likely
// a bug in the `unshare` crate). Using a semaphore at the test level seemed to
// be the only effective remedy-- even using a semaphore before calling the
// function that uses `unshare` somehow didn't fix it!
//
// In practice, this bug really only affects tests, and possibly only when
// multiple `Brioche` instances are used concurrently. In binary releases,
// we only ever have one `Brioche` instance, and we additionally use
// `unshare` through a child process, so it seems like we're unlikely to hit
// the same bug.
static TEST_SEMAPHORE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[tokio::test]
async fn test_resolve_process() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;

    let hello_process = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    assert_eq!(
        resolve_without_meta(&brioche, hello_process).await?,
        brioche_test::file(hello_blob, false),
    );

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_fail_on_no_output() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("# ... doing nothing ...")],
        env: BTreeMap::new(),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    assert_matches!(resolve_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_fail_on_non_zero_exit() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("echo -n hello > $BRIOCHE_OUTPUT && exit 1"),
        ],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    assert_matches!(resolve_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_command_no_path() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = LazyValue::Process(ProcessValue {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    assert_matches!(resolve_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

// For now, the `command` for a process is not resolved using `$PATH`. This
// is how `unshare` works, since it uses `execve` to run the command rather
// than `execvpe`
#[tokio::test]
async fn test_resolve_process_command_path() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = LazyValue::Process(ProcessValue {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("PATH".into(), tpl("/usr/bin")),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    assert_matches!(resolve_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_with_utils() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("touch $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    assert_matches!(resolve_without_meta(&brioche, process).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_cached() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process_random = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("cat /dev/urandom | head -c 1024 > $BRIOCHE_OUTPUT"),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    // Even though the process is non-deterministic, the result of the first
    // execution should be cached. So, even if we re-evaluate the process,
    // the result should be cached.

    let random_1 = resolve_without_meta(&brioche, process_random.clone()).await?;
    let random_2 = resolve_without_meta(&brioche, process_random).await?;

    assert_eq!(random_1, random_2);

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_cached_equivalent_inputs() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let empty_dir_1 = brioche_test::lazy_dir_empty();
    let empty_dir_2 = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("mkdir \"$BRIOCHE_OUTPUT\"")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    let empty_dir_1_resolved = resolve_without_meta(&brioche, empty_dir_1.clone()).await?;
    let empty_dir_2_resolved = resolve_without_meta(&brioche, empty_dir_2.clone()).await?;
    assert_eq!(empty_dir_1_resolved, empty_dir_2_resolved);

    let process_random_1 = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("echo $input >> $BRIOCHE_OUTPUT; cat /dev/urandom | head -c 1024 >> $BRIOCHE_OUTPUT"),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("input".into(), template_input(empty_dir_1)),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    let process_random_2 = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("echo $input >> $BRIOCHE_OUTPUT; cat /dev/urandom | head -c 1024 >> $BRIOCHE_OUTPUT"),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("input".into(), template_input(empty_dir_2)),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    // Both processes are different, but their inputs resolve to identical
    // values, so this should be a cache hit.

    let random_1 = resolve_without_meta(&brioche, process_random_1).await?;
    let random_2 = resolve_without_meta(&brioche, process_random_2).await?;

    assert_eq!(random_1, random_2);

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_cached_equivalent_inputs_parallel() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let empty_dir_1 = brioche_test::lazy_dir_empty();
    let empty_dir_2 = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("mkdir \"$BRIOCHE_OUTPUT\"")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    let empty_dir_1_resolved = resolve_without_meta(&brioche, empty_dir_1.clone()).await?;
    let empty_dir_2_resolved = resolve_without_meta(&brioche, empty_dir_2.clone()).await?;
    assert_eq!(empty_dir_1_resolved, empty_dir_2_resolved);

    let process_random_1 = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("echo $input >> $BRIOCHE_OUTPUT; cat /dev/urandom | head -c 1024 >> $BRIOCHE_OUTPUT"),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("input".into(), template_input(empty_dir_1)),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    let process_random_2 = LazyValue::Process(ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("echo $input >> $BRIOCHE_OUTPUT; cat /dev/urandom | head -c 1024 >> $BRIOCHE_OUTPUT"),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("input".into(), template_input(empty_dir_2)),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    });

    let process_random_1_proxy =
        brioche::brioche::resolve::create_proxy(&brioche, process_random_1.clone()).await;
    let process_random_2_proxy =
        brioche::brioche::resolve::create_proxy(&brioche, process_random_2.clone()).await;
    let processes_dir = brioche_test::lazy_dir([
        ("process1.txt", process_random_1.clone()),
        ("process2.txt", process_random_2.clone()),
        ("process1p.txt", process_random_1_proxy),
        ("process2p.txt", process_random_2_proxy),
    ]);
    let process_a_dir = brioche_test::lazy_dir([("a", processes_dir.clone())]);
    let process_b_dir = brioche_test::lazy_dir([("b", processes_dir.clone())]);
    let merged = LazyValue::Merge {
        directories: vec![
            brioche_test::without_meta(process_a_dir),
            brioche_test::without_meta(process_b_dir),
        ],
    };

    // Both processes are different, but their inputs resolve to identical
    // values, so this should be a cache hit.

    let resolved = resolve_without_meta(&brioche, merged).await?;
    let CompleteValue::Directory(resolved) = resolved else {
        panic!("expected directory");
    };

    let resolved_a1 = resolved.get(b"a/process1.txt")?.unwrap();
    let resolved_a1p = resolved.get(b"a/process1p.txt")?.unwrap();
    let resolved_b1 = resolved.get(b"b/process1.txt")?.unwrap();
    let resolved_b1p = resolved.get(b"b/process1p.txt")?.unwrap();

    let resolved_a2 = resolved.get(b"a/process2.txt")?.unwrap();
    let resolved_a2p = resolved.get(b"a/process2p.txt")?.unwrap();
    let resolved_b2 = resolved.get(b"b/process2.txt")?.unwrap();
    let resolved_b2p = resolved.get(b"b/process2p.txt")?.unwrap();

    assert_eq!(resolved_a1, resolved_a1p);
    assert_eq!(resolved_a1, resolved_b1);
    assert_eq!(resolved_a1, resolved_b1p);

    assert_eq!(resolved_a2, resolved_a2p);
    assert_eq!(resolved_a2, resolved_b2);
    assert_eq!(resolved_a2, resolved_b2p);

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_cache_busted() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process_random_1 = ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("cat /dev/urandom | head -c 1024 > $BRIOCHE_OUTPUT"),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    };
    let mut process_random_2 = process_random_1.clone();
    process_random_2
        .env
        .insert("WATERMARK".into(), tpl(r"¯\_(ツ)_/¯"));

    // Since the process is different due to a new env var, the cached result
    // from the first execution should not be used.

    let random_1 = resolve_without_meta(&brioche, LazyValue::Process(process_random_1)).await?;
    let random_2 = resolve_without_meta(&brioche, LazyValue::Process(process_random_2)).await?;

    assert_ne!(random_1, random_2);

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_custom_env_vars() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                test -n "$custom_output"
                test -d "$custom_home"
                test -d "$custom_resources"
                test -d "$custom_work_dir"
                test -d "$custom_temp_dir"

                test "$(pwd)" == "$custom_work_dir"

                touch "$custom_home/file.txt"
                touch "$custom_resources/file2.txt"
                touch "$custom_work_dir/file3.txt"
                touch "$custom_temp_dir/file3.txt"
                touch "$custom_output"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("custom_output".into(), output_path()),
            ("custom_home".into(), home_path()),
            ("custom_resources".into(), resources_path()),
            ("custom_work_dir".into(), work_dir_path()),
            ("custom_temp_dir".into(), temp_dir_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    };

    resolve_without_meta(&brioche, LazyValue::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_no_default_env_vars() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -eo pipefail

                test -z "$BRIOCHE_OUTPUT"
                test -z "$HOME"
                test -z "$BRIOCHE_PACK_RESOURCES_DIR"
                test -z "$TMPDIR"

                touch "$custom_output"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("custom_output".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    };

    resolve_without_meta(&brioche, LazyValue::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_starts_with_work_dir_contents() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;

    let process = ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                test "$(cat file.txt)" == "hello"
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_PACK_RESOURCES_DIR".into(), resources_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir([(
            "file.txt",
            brioche_test::lazy_file(hello_blob, false),
        )]))),
        platform: current_platform(),
    };

    resolve_without_meta(&brioche, LazyValue::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_has_resource_dir() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                test -d "$BRIOCHE_PACK_RESOURCES_DIR"
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_PACK_RESOURCES_DIR".into(), resources_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    };

    resolve_without_meta(&brioche, LazyValue::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_contains_all_resources() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let fizz = brioche_test::dir([(
        "file.txt",
        brioche_test::file_with_resources(
            brioche_test::blob(&brioche, b"foo").await,
            false,
            brioche_test::dir_value([(
                "foo/bar.txt",
                brioche_test::file(brioche_test::blob(&brioche, b"resource a").await, false),
            )]),
        ),
    )]);

    let buzz = brioche_test::file_with_resources(
        brioche_test::blob(&brioche, b"bar").await,
        false,
        brioche_test::dir_value([(
            "foo/baz.txt",
            brioche_test::file(brioche_test::blob(&brioche, b"resource b").await, false),
        )]),
    );

    let process = ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                test "$(cat "$fizz/file.txt")" = "foo"
                test "$(cat "$buzz")" = "bar"
                test "$(cat "$BRIOCHE_PACK_RESOURCES_DIR/foo/bar.txt")" = "resource a"
                test "$(cat "$BRIOCHE_PACK_RESOURCES_DIR/foo/baz.txt")" = "resource b"
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_PACK_RESOURCES_DIR".into(), resources_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("fizz".into(), template_input(fizz.into())),
            ("buzz".into(), template_input(buzz.into())),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    };

    resolve_without_meta(&brioche, LazyValue::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_resolve_process_output_with_resources() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    // Create a dummy file with pack metadata. This attaches resources
    // to the file when it gets put into the output of a process.
    let dummy_packed_contents = "dummy_packed".to_string();
    let mut dummy_packed_contents = dummy_packed_contents.into_bytes();
    brioche_pack::inject_pack(
        &mut dummy_packed_contents,
        &brioche_pack::Pack {
            program: "program".into(),
            interpreter: Some(brioche_pack::Interpreter::LdLinux {
                path: "ld-linux.so".into(),
                library_paths: vec![],
            }),
        },
    )?;

    let dummy_packed_blob = brioche_test::blob(&brioche, &dummy_packed_contents).await;
    let dummy_packed = brioche_test::file(dummy_packed_blob, false);

    let process = ProcessValue {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                mkdir -p "$BRIOCHE_OUTPUT/bin"
                echo -n "dummy_packed" > packed
                echo -n "dummy_program" > "$BRIOCHE_PACK_RESOURCES_DIR/program"
                echo -n "dummy_ld_linux" > "$BRIOCHE_PACK_RESOURCES_DIR/ld-linux.so"

                cp "$dummy_packed" "$BRIOCHE_OUTPUT/bin/program"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_PACK_RESOURCES_DIR".into(), resources_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("dummy_packed".into(), template_input(dummy_packed.into())),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        platform: current_platform(),
    };

    let result = resolve_without_meta(&brioche, LazyValue::Process(process)).await?;
    let CompleteValue::Directory(dir) = result else {
        panic!("expected directory");
    };

    let program = dir
        .get(b"bin/program")
        .unwrap()
        .expect("bin/program not found");
    let CompleteValue::File(File { resources, .. }) = &program.value else {
        panic!("expected file");
    };

    assert_eq!(
        *resources,
        brioche_test::dir_value([
            (
                "program",
                brioche_test::file(brioche_test::blob(&brioche, b"dummy_program").await, false)
            ),
            (
                "ld-linux.so",
                brioche_test::file(brioche_test::blob(&brioche, b"dummy_ld_linux").await, false)
            ),
        ])
    );

    Ok(())
}
