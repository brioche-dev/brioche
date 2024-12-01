#![cfg(target_os = "linux")]

use std::{collections::BTreeMap, sync::Mutex};

use anyhow::Context;
use assert_matches::assert_matches;
use pretty_assertions::assert_eq;

use brioche_core::{
    platform::current_platform,
    recipe::{
        ArchiveFormat, Artifact, CompressionFormat, Directory, DownloadRecipe, File, ProcessRecipe,
        Recipe, Unarchive, WithMeta,
    },
    Hash,
};
use brioche_test_support::{
    bake_without_meta, default_process, home_dir, input_resource_dirs, output_path, resource_dir,
    temp_dir, template_input, tpl, tpl_join, work_dir,
};

fn sha256_hash(hash: &str) -> Hash {
    Hash::Sha256 {
        value: hex::decode(hash).unwrap(),
    }
}

fn utils() -> Recipe {
    let utils_download = Recipe::Download(DownloadRecipe {
        url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/utils_amd64_linux.tar.zstd".parse().unwrap(),
        hash: sha256_hash("eb29ea059fcd9ca457841f5c79151721a74761a31610d694bce61a62f4de6d33"),
    });

    Recipe::Unarchive(Unarchive {
        file: Box::new(brioche_test_support::without_meta(utils_download)),
        archive: ArchiveFormat::Tar,
        compression: CompressionFormat::Zstd,
    })
}

async fn try_get(
    brioche: &brioche_core::Brioche,
    dir: &Directory,
    path: impl AsRef<[u8]>,
) -> anyhow::Result<Option<Artifact>> {
    let artifact = dir.get(brioche, path.as_ref()).await?;
    Ok(artifact)
}

async fn get(brioche: &brioche_core::Brioche, dir: &Directory, path: impl AsRef<[u8]>) -> Artifact {
    let path = bstr::BStr::new(path.as_ref());
    try_get(brioche, dir, &path)
        .await
        .with_context(|| format!("error getting context for path {path:?}",))
        .unwrap()
        .with_context(|| format!("no artifact found for path {path:?}"))
        .unwrap()
}

#[allow(dead_code)]
struct ProcessTestLock(std::sync::MutexGuard<'static, ()>);

async fn brioche_test() -> (
    brioche_core::Brioche,
    brioche_test_support::TestContext,
    ProcessTestLock,
) {
    static LOCK: Mutex<()> = Mutex::new(());
    let lock = ProcessTestLock(LOCK.lock().unwrap());

    let (brioche, context) = brioche_test_support::brioche_test().await;

    brioche_test_support::load_rootfs_recipes(&brioche, current_platform()).await;

    (brioche, context, lock)
}

#[tokio::test]
async fn test_bake_process_simple() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let hello = "hello";
    let hello_blob = brioche_test_support::blob(&brioche, hello).await;

    let hello_process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(&brioche, hello_process).await?,
        brioche_test_support::file(hello_blob, false),
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_fail_on_no_output() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("# ... doing nothing ...")],
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_scaffold_output() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let hello_blob = brioche_test_support::blob(&brioche, "hello").await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("# ... doing nothing ...")],
        output_scaffold: Some(Box::new(WithMeta::without_meta(
            brioche_test_support::lazy_file(hello_blob, false),
        ))),
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(&brioche, process).await?,
        brioche_test_support::file(hello_blob, false)
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_scaffold_and_modify_output() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let hello_blob = brioche_test_support::blob(&brioche, "hello").await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"echo -n hello > "$BRIOCHE_OUTPUT/hi.txt""#),
        ],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        output_scaffold: Some(Box::new(WithMeta::without_meta(
            brioche_test_support::lazy_dir_empty(),
        ))),
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(&brioche, process).await?,
        brioche_test_support::dir(
            &brioche,
            [("hi.txt", brioche_test_support::file(hello_blob, false)),]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_fail_on_non_zero_exit() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("echo -n hello > $BRIOCHE_OUTPUT && exit 1"),
        ],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_command_no_path() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

// For now, the `command` for a process is not resolved using `$PATH`. This
// is how `unshare` works, since it uses `execve` to run the command rather

#[tokio::test] // than `execvpe`
async fn test_bake_process_command_path() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("PATH".into(), tpl("/usr/bin")),
        ]),
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_with_utils() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("touch $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_with_readonly_contents() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                mkdir -p output/foo/bar &&
                echo 'hello' > output/foo/bar/baz.txt &&
                cp -r output "$BRIOCHE_OUTPUT" &&
                chmod -R a-w output "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_cached() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process_random = Recipe::Process(ProcessRecipe {
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
        ..default_process()
    });

    // Even though the process is non-deterministic, the result of the first
    // execution should be cached. So, even if we re-evaluate the process,
    // the result should be cached.

    let random_1 = bake_without_meta(&brioche, process_random.clone()).await?;
    let random_2 = bake_without_meta(&brioche, process_random).await?;

    assert_eq!(random_1, random_2);

    Ok(())
}

#[tokio::test]
async fn test_bake_process_cached_equivalent_inputs() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let empty_dir_1 = brioche_test_support::lazy_dir_empty();
    let empty_dir_2 = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("mkdir \"$BRIOCHE_OUTPUT\"")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        ..default_process()
    });

    let empty_dir_1_baked = bake_without_meta(&brioche, empty_dir_1.clone()).await?;
    let empty_dir_2_baked = bake_without_meta(&brioche, empty_dir_2.clone()).await?;
    assert_eq!(empty_dir_1_baked, empty_dir_2_baked);

    let process_random_1 = Recipe::Process(ProcessRecipe {
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
        ..default_process()
    });

    let process_random_2 = Recipe::Process(ProcessRecipe {
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
        ..default_process()
    });

    // Both processes are different, but their inputs bake to identical
    // artifacts, so this should be a cache hit.

    let random_1 = bake_without_meta(&brioche, process_random_1).await?;
    let random_2 = bake_without_meta(&brioche, process_random_2).await?;

    assert_eq!(random_1, random_2);

    Ok(())
}

#[tokio::test]
async fn test_bake_process_cached_equivalent_inputs_parallel() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let empty_dir_1 = brioche_test_support::lazy_dir_empty();
    let empty_dir_2 = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("mkdir \"$BRIOCHE_OUTPUT\"")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        ..default_process()
    });

    let empty_dir_1_baked = bake_without_meta(&brioche, empty_dir_1.clone()).await?;
    let empty_dir_2_baked = bake_without_meta(&brioche, empty_dir_2.clone()).await?;
    assert_eq!(empty_dir_1_baked, empty_dir_2_baked);

    let process_random_1 = Recipe::Process(ProcessRecipe {
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
        ..default_process()
    });

    let process_random_2 = Recipe::Process(ProcessRecipe {
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
        ..default_process()
    });

    let process_random_1_proxy =
        brioche_core::bake::create_proxy(&brioche, process_random_1.clone()).await?;
    let process_random_2_proxy =
        brioche_core::bake::create_proxy(&brioche, process_random_2.clone()).await?;
    let processes_dir = brioche_test_support::lazy_dir([
        ("process1.txt", process_random_1.clone()),
        ("process2.txt", process_random_2.clone()),
        ("process1p.txt", process_random_1_proxy),
        ("process2p.txt", process_random_2_proxy),
    ]);
    let process_a_dir = brioche_test_support::lazy_dir([("a", processes_dir.clone())]);
    let process_b_dir = brioche_test_support::lazy_dir([("b", processes_dir.clone())]);
    let merged = Recipe::Merge {
        directories: vec![
            brioche_test_support::without_meta(process_a_dir),
            brioche_test_support::without_meta(process_b_dir),
        ],
    };

    // Both processes are different, but their inputs bake to identical
    // artifacts, so this should be a cache hit.

    let baked = bake_without_meta(&brioche, merged).await?;
    let Artifact::Directory(baked) = baked else {
        panic!("expected directory");
    };

    let baked_a1 = get(&brioche, &baked, "a/process1.txt").await;
    let baked_a1p = get(&brioche, &baked, "a/process1p.txt").await;
    let baked_b1 = get(&brioche, &baked, "b/process1.txt").await;
    let baked_b1p = get(&brioche, &baked, "b/process1p.txt").await;

    let baked_a2 = get(&brioche, &baked, "a/process2.txt").await;
    let baked_a2p = get(&brioche, &baked, "a/process2p.txt").await;
    let baked_b2 = get(&brioche, &baked, "b/process2.txt").await;
    let baked_b2p = get(&brioche, &baked, "b/process2p.txt").await;

    assert_eq!(baked_a1, baked_a1p);
    assert_eq!(baked_a1, baked_b1);
    assert_eq!(baked_a1, baked_b1p);

    assert_eq!(baked_a2, baked_a2p);
    assert_eq!(baked_a2, baked_b2);
    assert_eq!(baked_a2, baked_b2p);

    Ok(())
}

#[tokio::test]
async fn test_bake_process_cache_busted() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process_random_1 = ProcessRecipe {
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
        ..default_process()
    };
    let mut process_random_2 = process_random_1.clone();
    process_random_2
        .env
        .insert("WATERMARK".into(), tpl(r"¯\_(ツ)_/¯"));

    // Since the process is different due to a new env var, the cached result
    // from the first execution should not be used.

    let random_1 = bake_without_meta(&brioche, Recipe::Process(process_random_1)).await?;
    let random_2 = bake_without_meta(&brioche, Recipe::Process(process_random_2)).await?;

    assert_ne!(random_1, random_2);

    Ok(())
}

#[tokio::test]
async fn test_bake_process_custom_env_vars() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = ProcessRecipe {
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
            ("custom_home".into(), home_dir()),
            ("custom_resources".into(), resource_dir()),
            ("custom_work_dir".into(), work_dir()),
            ("custom_temp_dir".into(), temp_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_no_default_env_vars() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -eo pipefail

                test -z "$BRIOCHE_OUTPUT"
                test -z "$HOME"
                test -z "$BRIOCHE_RESOURCE_DIR"
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
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_no_default_path() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = ProcessRecipe {
        command: tpl("sh"),
        args: vec![
            tpl("-c"),
            tpl(r#"
                set -eo pipefail
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            // $PATH not set
        ]),
        ..default_process()
    };

    assert_matches!(
        bake_without_meta(&brioche, Recipe::Process(process)).await,
        Err(_)
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_command_ambiguous_error() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;
    // Using a shorthand for the command ("sh") is only allowed if
    // each element in `$PATH` is an artifact subpath (e.g. `${artifact}/bin`)
    let process = ProcessRecipe {
        command: tpl("sh"),
        args: vec![
            tpl("-c"),
            tpl(r#"
                set -eo pipefail
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([tpl("/usr/bin:"), template_input(utils()), tpl("/bin")]),
            ),
        ]),
        ..default_process()
    };

    assert_matches!(
        bake_without_meta(&brioche, Recipe::Process(process)).await,
        Err(_)
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_command_uses_path() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = ProcessRecipe {
        command: tpl("sh"),
        args: vec![
            tpl("-c"),
            tpl(r#"
                set -eo pipefail
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("BRIOCHE_OUTPUT".into(), output_path()),
        ]),
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_command_uses_dependencies() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = ProcessRecipe {
        command: tpl("sh"),
        args: vec![
            tpl("-c"),
            tpl(r#"
                set -eo pipefail
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        dependencies: vec![WithMeta::without_meta(utils())],
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_starts_with_work_dir_contents() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let hello_blob = brioche_test_support::blob(&brioche, b"hello").await;

    let process = ProcessRecipe {
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
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test_support::without_meta(
            brioche_test_support::lazy_dir([(
                "file.txt",
                brioche_test_support::lazy_file(hello_blob, false),
            )]),
        )),
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_edit_work_dir_contents() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let hello_blob = brioche_test_support::blob(&brioche, b"hello").await;

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                echo -n new > file.txt
                echo -n new2 > dir/other.txt
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test_support::without_meta(
            brioche_test_support::lazy_dir([
                (
                    "file.txt",
                    brioche_test_support::lazy_file(hello_blob, false),
                ),
                (
                    "dir",
                    brioche_test_support::lazy_dir([(
                        "other.txt",
                        brioche_test_support::lazy_file(hello_blob, false),
                    )]),
                ),
            ]),
        )),
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_has_resource_dir() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                test -d "$BRIOCHE_RESOURCE_DIR"
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_contains_all_resources() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let fizz = brioche_test_support::dir(
        &brioche,
        [(
            "file.txt",
            brioche_test_support::file_with_resources(
                brioche_test_support::blob(&brioche, b"foo").await,
                false,
                brioche_test_support::dir_value(
                    &brioche,
                    [(
                        "foo/bar.txt",
                        brioche_test_support::file(
                            brioche_test_support::blob(&brioche, b"resource a").await,
                            false,
                        ),
                    )],
                )
                .await,
            ),
        )],
    )
    .await;

    let buzz = brioche_test_support::file_with_resources(
        brioche_test_support::blob(&brioche, b"bar").await,
        false,
        brioche_test_support::dir_value(
            &brioche,
            [(
                "foo/baz.txt",
                brioche_test_support::file(
                    brioche_test_support::blob(&brioche, b"resource b").await,
                    false,
                ),
            )],
        )
        .await,
    );

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                test "$(cat "$fizz/file.txt")" = "foo"
                test "$(cat "$buzz")" = "bar"
                bar=""
                baz=""
                oldifs="$IFS"
                IFS=":"
                for dir in $BRIOCHE_INPUT_RESOURCE_DIRS; do
                    if [ -e "$dir/foo/bar.txt" ]; then
                        bar="$(cat "$dir/foo/bar.txt")"
                    fi
                    if [ -e "$dir/foo/baz.txt" ]; then
                        baz="$(cat "$dir/foo/baz.txt")"
                    fi
                done
                IFS="$oldifs"
                test "$bar" = "resource a"
                test "$baz" = "resource b"
                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_INPUT_RESOURCE_DIRS".into(), input_resource_dirs()),
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("fizz".into(), template_input(fizz.into())),
            ("buzz".into(), template_input(buzz.into())),
        ]),
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_output_with_resources() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;
    // Create a dummy file with pack metadata. This attaches resources
    // to the file when it gets put into the output of a process.
    let dummy_packed_contents = "dummy_packed".to_string();
    let mut dummy_packed_contents = dummy_packed_contents.into_bytes();
    brioche_pack::inject_pack(
        &mut dummy_packed_contents,
        &brioche_pack::Pack::LdLinux {
            program: "program".into(),
            interpreter: "ld-linux.so".into(),
            library_dirs: vec![],
            runtime_library_dirs: vec![],
        },
    )?;

    let dummy_packed_blob = brioche_test_support::blob(&brioche, &dummy_packed_contents).await;
    let dummy_packed = brioche_test_support::file(dummy_packed_blob, false);

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                mkdir -p "$BRIOCHE_OUTPUT/bin"
                echo -n "dummy_packed" > packed
                echo -n "dummy_program" > "$BRIOCHE_RESOURCE_DIR/program"
                echo -n "dummy_ld_linux" > "$BRIOCHE_RESOURCE_DIR/ld-linux.so"

                cp "$dummy_packed" "$BRIOCHE_OUTPUT/bin/program"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("dummy_packed".into(), template_input(dummy_packed.into())),
        ]),
        ..default_process()
    };

    let result = bake_without_meta(&brioche, Recipe::Process(process)).await?;
    let Artifact::Directory(dir) = result else {
        panic!("expected directory");
    };

    let program = get(&brioche, &dir, "bin/program").await;
    let Artifact::File(File { resources, .. }) = &program else {
        panic!("expected file");
    };

    assert_eq!(
        *resources,
        brioche_test_support::dir_value(
            &brioche,
            [
                (
                    "program",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_program").await,
                        false
                    )
                ),
                (
                    "ld-linux.so",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_ld_linux").await,
                        false
                    )
                ),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_output_with_shared_resources() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;
    // Create a dummy file with pack metadata. This attaches resources
    // to the file when it gets put into the output of a process.
    let dummy_packed_1_contents = "dummy_packed_1".to_string();
    let mut dummy_packed_1_contents = dummy_packed_1_contents.into_bytes();
    brioche_pack::inject_pack(
        &mut dummy_packed_1_contents,
        &brioche_pack::Pack::Metadata {
            resource_paths: vec![
                "resource_1".into(),
                "foo".into(),
                "bar".into(),
                "baz".into(),
                "quux".into(),
            ],
            format: Default::default(),
            metadata: Default::default(),
        },
    )?;

    let dummy_packed_1_blob = brioche_test_support::blob(&brioche, &dummy_packed_1_contents).await;
    let dummy_packed_1 = brioche_test_support::file(dummy_packed_1_blob, false);

    // Sane as above
    let dummy_packed_2_contents = "dummy_packed_2".to_string();
    let mut dummy_packed_2_contents = dummy_packed_2_contents.into_bytes();
    brioche_pack::inject_pack(
        &mut dummy_packed_2_contents,
        &brioche_pack::Pack::Metadata {
            resource_paths: vec![
                "resource_2".into(),
                "foo".into(),
                "bar".into(),
                "baz".into(),
                "quux".into(),
            ],
            format: Default::default(),
            metadata: Default::default(),
        },
    )?;

    let dummy_packed_2_blob = brioche_test_support::blob(&brioche, &dummy_packed_2_contents).await;
    let dummy_packed_2 = brioche_test_support::file(dummy_packed_2_blob, false);

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                mkdir -p "$BRIOCHE_OUTPUT/bin"

                echo -n "resource_1_target" > "$BRIOCHE_RESOURCE_DIR/resource_1_target"
                echo -n "resource_2_target" > "$BRIOCHE_RESOURCE_DIR/resource_2_target"

                echo -n "dummy_foo" > "$BRIOCHE_RESOURCE_DIR/foo"
                echo -n "dummy_bar_target" > "$BRIOCHE_RESOURCE_DIR/bar_target"
                echo -n "dummy_baz_target" > "$BRIOCHE_RESOURCE_DIR/baz_target"

                ln -s "resource_1_target" "$BRIOCHE_RESOURCE_DIR/resource_1"
                ln -s "resource_2_target" "$BRIOCHE_RESOURCE_DIR/resource_2"

                ln -s "bar_target" "$BRIOCHE_RESOURCE_DIR/bar"
                ln -s "baz_target" "$BRIOCHE_RESOURCE_DIR/baz"

                cp "$dummy_packed_1" "$BRIOCHE_OUTPUT/bin/program_1"
                cp "$dummy_packed_2" "$BRIOCHE_OUTPUT/bin/program_2"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            (
                "dummy_packed_1".into(),
                template_input(dummy_packed_1.into()),
            ),
            (
                "dummy_packed_2".into(),
                template_input(dummy_packed_2.into()),
            ),
        ]),
        ..default_process()
    };

    let result = bake_without_meta(&brioche, Recipe::Process(process)).await?;
    let Artifact::Directory(dir) = result else {
        panic!("expected directory");
    };

    let program_1 = get(&brioche, &dir, "bin/program_1").await;
    let Artifact::File(File {
        resources: resources_1,
        ..
    }) = &program_1
    else {
        panic!("expected file");
    };

    let program_2 = get(&brioche, &dir, "bin/program_2").await;
    let Artifact::File(File {
        resources: resources_2,
        ..
    }) = &program_2
    else {
        panic!("expected file");
    };

    assert_eq!(
        *resources_1,
        brioche_test_support::dir_value(
            &brioche,
            [
                (
                    "resource_1",
                    brioche_test_support::symlink("resource_1_target"),
                ),
                (
                    "resource_1_target",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"resource_1_target").await,
                        false
                    )
                ),
                (
                    "foo",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_foo").await,
                        false
                    )
                ),
                ("bar", brioche_test_support::symlink("bar_target"),),
                (
                    "bar_target",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_bar_target").await,
                        false
                    )
                ),
                ("baz", brioche_test_support::symlink("baz_target")),
                (
                    "baz_target",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_baz_target").await,
                        false
                    ),
                ),
            ]
        )
        .await
    );

    assert_eq!(
        *resources_2,
        brioche_test_support::dir_value(
            &brioche,
            [
                (
                    "resource_2",
                    brioche_test_support::symlink("resource_2_target"),
                ),
                (
                    "resource_2_target",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"resource_2_target").await,
                        false
                    )
                ),
                (
                    "foo",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_foo").await,
                        false
                    )
                ),
                ("bar", brioche_test_support::symlink("bar_target"),),
                (
                    "bar_target",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_bar_target").await,
                        false
                    )
                ),
                ("baz", brioche_test_support::symlink("baz_target")),
                (
                    "baz_target",
                    brioche_test_support::file(
                        brioche_test_support::blob(&brioche, b"dummy_baz_target").await,
                        false
                    ),
                ),
            ]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_output_with_nested_symlink_resource_error() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;
    // Create a dummy file with pack metadata. This attaches resources
    // to the file when it gets put into the output of a process.
    let dummy_packed_contents = "dummy_packed".to_string();
    let mut dummy_packed_contents = dummy_packed_contents.into_bytes();
    brioche_pack::inject_pack(
        &mut dummy_packed_contents,
        &brioche_pack::Pack::Metadata {
            resource_paths: vec!["foo".into()],
            format: Default::default(),
            metadata: Default::default(),
        },
    )?;

    let dummy_packed_blob = brioche_test_support::blob(&brioche, &dummy_packed_contents).await;
    let dummy_packed = brioche_test_support::file(dummy_packed_blob, false);

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                mkdir -p "$BRIOCHE_OUTPUT/bin"

                echo -n "dummy_foo_target_target" > "$BRIOCHE_RESOURCE_DIR/foo_target_target"
                ln -s "foo_target_target" "$BRIOCHE_RESOURCE_DIR/foo_target"
                ln -s "foo_target" "$BRIOCHE_RESOURCE_DIR/foo"

                cp "$dummy_packed" "$BRIOCHE_OUTPUT/bin/program"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("dummy_packed".into(), template_input(dummy_packed.into())),
        ]),
        ..default_process()
    };

    let result = bake_without_meta(&brioche, Recipe::Process(process)).await;

    // Symlink resource points to another resource, which is not allowed
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_output_with_symlink_traversal_resource_error() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;
    // Create a dummy file with pack metadata. This attaches resources
    // to the file when it gets put into the output of a process.
    let dummy_packed_contents = "dummy_packed".to_string();
    let mut dummy_packed_contents = dummy_packed_contents.into_bytes();
    brioche_pack::inject_pack(
        &mut dummy_packed_contents,
        &brioche_pack::Pack::Metadata {
            resource_paths: vec!["foo".into()],
            format: Default::default(),
            metadata: Default::default(),
        },
    )?;

    let dummy_packed_blob = brioche_test_support::blob(&brioche, &dummy_packed_contents).await;
    let dummy_packed = brioche_test_support::file(dummy_packed_blob, false);

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail
                mkdir -p "$BRIOCHE_OUTPUT/bin"

                mkdir -p "$BRIOCHE_RESOURCE_DIR/fizz_target"
                echo -n "dummy_fizzbuzz" > "$BRIOCHE_RESOURCE_DIR/fizz_target/buzz"
                ln -s "fizz_target" "$BRIOCHE_RESOURCE_DIR/fizz"
                ln -s "fizz/buzz" "$BRIOCHE_RESOURCE_DIR/foo"

                cp "$dummy_packed" "$BRIOCHE_OUTPUT/bin/program"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("BRIOCHE_RESOURCE_DIR".into(), resource_dir()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("dummy_packed".into(), template_input(dummy_packed.into())),
        ]),
        ..default_process()
    };

    let result = bake_without_meta(&brioche, Recipe::Process(process)).await;

    // Symlink resource traverses a symlink, which is not allowed
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_unsafe_validation() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let no_unsafety = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        platform: current_platform(),
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, no_unsafety).await, Ok(_));

    let needs_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        is_unsafe: false,
        networking: true,
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, needs_unsafe).await, Err(_));

    let unnecessary_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        is_unsafe: true,
        networking: false,
        ..default_process()
    });

    assert_matches!(
        bake_without_meta(&brioche, unnecessary_unsafe).await,
        Err(_)
    );

    let necessary_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        is_unsafe: true,
        networking: true,
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, necessary_unsafe).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_networking_disabled() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let mut server = mockito::Server::new_async().await;
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body("hello")
        .expect(0)
        .create();

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                wget \
                    --timeout=1 \
                    -O "$BRIOCHE_OUTPUT" \
                    "$URL/file.txt" \
                    > /dev/null 2> /dev/null
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("URL".into(), tpl(server.url())),
        ]),
        is_unsafe: false,
        networking: false,
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_process_networking_enabled() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let mut server = mockito::Server::new_async().await;
    let hello_endpoint = server
        .mock("GET", "/file.txt")
        .with_body("hello")
        .expect(1)
        .create();

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                wget \
                    --timeout=1 \
                    -O "$BRIOCHE_OUTPUT" \
                    "$URL/file.txt" \
                    > /dev/null 2> /dev/null
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("URL".into(), tpl(server.url())),
        ]),
        is_unsafe: true,
        networking: true,
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(&brioche, process).await?,
        brioche_test_support::file(brioche_test_support::blob(&brioche, "hello").await, false),
    );

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_process_networking_enabled_dns() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                wget \
                    --timeout=1 \
                    -O "$BRIOCHE_OUTPUT" \
                    "https://gist.githubusercontent.com/kylewlacy/c0f1a43e2641686f377178880fcce6ae/raw/f48155695445aa218e558fba824b61cf718d5e55/lorem-ipsum.txt" \
                    > /dev/null 2> /dev/null
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        is_unsafe: true,
        networking: true,
        ..default_process()
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_dependencies() -> anyhow::Result<()> {
    let (brioche, _context, _lock) = brioche_test().await;

    let dep1 = brioche_test_support::dir(
        &brioche,
        [
            ("a", brioche_test_support::dir_empty()),
            ("b", brioche_test_support::dir_empty()),
            ("c", brioche_test_support::dir_empty()),
            ("bin", brioche_test_support::dir_empty()),
            (
                "brioche-env.d",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "env",
                        brioche_test_support::dir(
                            &brioche,
                            [
                                (
                                    "PATH",
                                    brioche_test_support::dir(
                                        &brioche,
                                        [
                                            ("a", brioche_test_support::symlink(b"../../../a")),
                                            ("b", brioche_test_support::symlink(b"../../../b")),
                                        ],
                                    )
                                    .await,
                                ),
                                (
                                    "DEP1",
                                    brioche_test_support::dir(
                                        &brioche,
                                        [("a", brioche_test_support::symlink(b"../../../c"))],
                                    )
                                    .await,
                                ),
                                ("DEP1_SYMLINK", brioche_test_support::symlink(b"../../a")),
                                (
                                    "DEP1_FILE",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep1_file").await,
                                        false,
                                    ),
                                ),
                                (
                                    "FALLBACK_1",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep1_fallback1")
                                            .await,
                                        false,
                                    ),
                                ),
                                (
                                    "FALLBACK_2",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep1_fallback2")
                                            .await,
                                        false,
                                    ),
                                ),
                            ],
                        )
                        .await,
                    )],
                )
                .await,
            ),
        ],
    )
    .await;
    let dep2 = brioche_test_support::dir(
        &brioche,
        [
            ("d", brioche_test_support::dir_empty()),
            ("e", brioche_test_support::dir_empty()),
            ("f", brioche_test_support::dir_empty()),
            ("bin", brioche_test_support::dir_empty()),
            (
                "brioche-env.d",
                brioche_test_support::dir(
                    &brioche,
                    [(
                        "env",
                        brioche_test_support::dir(
                            &brioche,
                            [
                                (
                                    "PATH",
                                    brioche_test_support::dir(
                                        &brioche,
                                        [
                                            ("d", brioche_test_support::symlink(b"../../../d")),
                                            ("e", brioche_test_support::symlink(b"../../../e")),
                                        ],
                                    )
                                    .await,
                                ),
                                (
                                    "DEP2",
                                    brioche_test_support::dir(
                                        &brioche,
                                        [("a", brioche_test_support::symlink(b"../../../f"))],
                                    )
                                    .await,
                                ),
                                ("DEP2_SYMLINK", brioche_test_support::symlink(b"../../d")),
                                (
                                    "DEP2_FILE",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep2_file").await,
                                        false,
                                    ),
                                ),
                                (
                                    "FALLBACK_1",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep2_fallback1")
                                            .await,
                                        false,
                                    ),
                                ),
                                (
                                    "FALLBACK_2",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep2_fallback2")
                                            .await,
                                        false,
                                    ),
                                ),
                                (
                                    "FALLBACK_3",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep2_fallback3")
                                            .await,
                                        false,
                                    ),
                                ),
                                (
                                    "FALLBACK_4",
                                    brioche_test_support::file(
                                        brioche_test_support::blob(&brioche, b"dep2_fallback4")
                                            .await,
                                        false,
                                    ),
                                ),
                            ],
                        )
                        .await,
                    )],
                )
                .await,
            ),
        ],
    )
    .await;

    let process = ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"
                set -euo pipefail

                test "$PATH" = "$EXPECTED_PATH"
                test "$DEP1" = "$EXPECTED_DEP1"
                test "$DEP2" = "$EXPECTED_DEP2"
                test "$DEP1_SYMLINK" = "$EXPECTED_DEP1_SYMLINK"
                test "$DEP1_FILE" = "$EXPECTED_DEP1_FILE"
                test "$DEP2_SYMLINK" = "$EXPECTED_DEP2_SYMLINK"
                test "$DEP2_FILE" = "$EXPECTED_DEP2_FILE"
                test "$FALLBACK_1" = "$EXPECTED_FALLBACK_1"
                test "$FALLBACK_2" = "$EXPECTED_FALLBACK_2"
                test "$FALLBACK_3" = "$EXPECTED_FALLBACK_3"
                test "$FALLBACK_4" = "$EXPECTED_FALLBACK_4"

                touch "$BRIOCHE_OUTPUT"
            "#),
        ],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
            ("DEP1".into(), tpl("hello")),
            ("FALLBACK_4".into(), tpl("process_fallback4")),
            (
                "EXPECTED_PATH".into(),
                tpl_join([
                    template_input(utils()),
                    tpl("/bin:"),
                    template_input(dep1.clone().into()),
                    tpl("/a:"),
                    template_input(dep1.clone().into()),
                    tpl("/b:"),
                    template_input(dep1.clone().into()),
                    tpl("/bin:"),
                    template_input(dep2.clone().into()),
                    tpl("/d:"),
                    template_input(dep2.clone().into()),
                    tpl("/e:"),
                    template_input(dep2.clone().into()),
                    tpl("/bin"),
                ]),
            ),
            (
                "EXPECTED_DEP1".into(),
                tpl_join([
                    tpl("hello:"),
                    template_input(dep1.clone().into()),
                    tpl("/c"),
                ]),
            ),
            (
                "EXPECTED_DEP1_SYMLINK".into(),
                tpl_join([template_input(dep1.clone().into()), tpl("/a")]),
            ),
            ("EXPECTED_DEP1_FILE".into(), tpl("dep1_file")),
            (
                "EXPECTED_DEP2".into(),
                tpl_join([template_input(dep2.clone().into()), tpl("/f")]),
            ),
            (
                "EXPECTED_DEP2_SYMLINK".into(),
                tpl_join([template_input(dep2.clone().into()), tpl("/d")]),
            ),
            ("EXPECTED_DEP2_FILE".into(), tpl("dep2_file")),
            ("EXPECTED_FALLBACK_1".into(), tpl("dep1_fallback1")),
            ("EXPECTED_FALLBACK_2".into(), tpl("dep1_fallback2")),
            ("EXPECTED_FALLBACK_3".into(), tpl("dep2_fallback3")),
            ("EXPECTED_FALLBACK_4".into(), tpl("process_fallback4")),
        ]),
        dependencies: vec![
            WithMeta::without_meta(dep1.clone().into()),
            WithMeta::without_meta(dep2.clone().into()),
        ],
        ..default_process()
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}
