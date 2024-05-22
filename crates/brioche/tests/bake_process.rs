#![cfg(target_os = "linux")]

use std::collections::BTreeMap;

use anyhow::Context;
use assert_matches::assert_matches;
use pretty_assertions::assert_eq;

use brioche::{
    platform::current_platform,
    recipe::{
        ArchiveFormat, Artifact, CompressionFormat, Directory, DownloadRecipe, File, ProcessRecipe,
        ProcessTemplate, ProcessTemplateComponent, Recipe, Unarchive, WithMeta,
    },
    Hash,
};
use brioche_test::bake_without_meta;

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

fn template_input(input: Recipe) -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::Input {
            recipe: brioche_test::without_meta(input),
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

fn utils() -> Recipe {
    let utils_download = Recipe::Download(DownloadRecipe {
        url: "https://development-content.brioche.dev/github.com/tangramdotdev/bootstrap/2023-07-06/utils_amd64_linux.tar.zstd".parse().unwrap(),
        hash: sha256_hash("eb29ea059fcd9ca457841f5c79151721a74761a31610d694bce61a62f4de6d33"),
    });

    Recipe::Unarchive(Unarchive {
        file: Box::new(brioche_test::without_meta(utils_download)),
        archive: ArchiveFormat::Tar,
        compression: CompressionFormat::Zstd,
    })
}

async fn try_get(
    brioche: &brioche::Brioche,
    dir: &Directory,
    path: impl AsRef<[u8]>,
) -> anyhow::Result<Option<WithMeta<Artifact>>> {
    let artifact = dir.get(brioche, path.as_ref()).await?;
    Ok(artifact)
}

async fn get(
    brioche: &brioche::Brioche,
    dir: &Directory,
    path: impl AsRef<[u8]>,
) -> WithMeta<Artifact> {
    let path = bstr::BStr::new(path.as_ref());
    try_get(brioche, dir, &path)
        .await
        .with_context(|| format!("error getting context for path {path:?}",))
        .unwrap()
        .with_context(|| format!("no artifact found for path {path:?}"))
        .unwrap()
}

// HACK: This semaphore is used to ensure only one process test runs at a time.
// For some reason, running all these tests concurrently causes the method
// `unshare::run::Command.spawn()` to hang (this happens about 5-10% of the
// time when running `cargo test 'test_bake_process'`). This is most likely
// a bug in the `unshare` crate. Using a semaphore at the test level seemed to
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
async fn test_bake_process() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello = "hello";
    let hello_blob = brioche_test::blob(&brioche, hello).await;

    let hello_process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_eq!(
        bake_without_meta(&brioche, hello_process).await?,
        brioche_test::file(hello_blob, false),
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_fail_on_no_output() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("# ... doing nothing ...")],
        env: BTreeMap::new(),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_scaffold_output() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, "hello").await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("# ... doing nothing ...")],
        env: BTreeMap::new(),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: Some(Box::new(WithMeta::without_meta(brioche_test::lazy_file(
            hello_blob, false,
        )))),
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_eq!(
        bake_without_meta(&brioche, process).await?,
        brioche_test::file(hello_blob, false)
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_scaffold_and_modify_output() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, "hello").await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"echo -n hello > "$BRIOCHE_OUTPUT/hi.txt""#),
        ],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: Some(Box::new(WithMeta::without_meta(
            brioche_test::lazy_dir_empty(),
        ))),
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_eq!(
        bake_without_meta(&brioche, process).await?,
        brioche_test::dir(
            &brioche,
            [("hi.txt", brioche_test::file(hello_blob, false)),]
        )
        .await
    );

    Ok(())
}

#[tokio::test]
async fn test_bake_process_fail_on_non_zero_exit() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl("echo -n hello > $BRIOCHE_OUTPUT && exit 1"),
        ],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_command_no_path() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

// For now, the `command` for a process is not resolved using `$PATH`. This
// is how `unshare` works, since it uses `execve` to run the command rather
// than `execvpe`
#[tokio::test]
async fn test_bake_process_command_path() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("PATH".into(), tpl("/usr/bin")),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_with_utils() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_with_readonly_contents() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_cached() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
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
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let empty_dir_1 = brioche_test::lazy_dir_empty();
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
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
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let empty_dir_1 = brioche_test::lazy_dir_empty();
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    let process_random_1_proxy =
        brioche::bake::create_proxy(&brioche, process_random_1.clone()).await?;
    let process_random_2_proxy =
        brioche::bake::create_proxy(&brioche, process_random_2.clone()).await?;
    let processes_dir = brioche_test::lazy_dir([
        ("process1.txt", process_random_1.clone()),
        ("process2.txt", process_random_2.clone()),
        ("process1p.txt", process_random_1_proxy),
        ("process2p.txt", process_random_2_proxy),
    ]);
    let process_a_dir = brioche_test::lazy_dir([("a", processes_dir.clone())]);
    let process_b_dir = brioche_test::lazy_dir([("b", processes_dir.clone())]);
    let merged = Recipe::Merge {
        directories: vec![
            brioche_test::without_meta(process_a_dir),
            brioche_test::without_meta(process_b_dir),
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
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
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
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

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
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_no_default_env_vars() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = ProcessRecipe {
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
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_starts_with_work_dir_contents() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;

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
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_edit_work_dir_contents() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let hello_blob = brioche_test::blob(&brioche, b"hello").await;

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
            ("BRIOCHE_PACK_RESOURCES_DIR".into(), resources_path()),
            (
                "PATH".into(),
                tpl_join([template_input(utils()), tpl("/bin")]),
            ),
        ]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir([
            ("file.txt", brioche_test::lazy_file(hello_blob, false)),
            (
                "dir",
                brioche_test::lazy_dir([("other.txt", brioche_test::lazy_file(hello_blob, false))]),
            ),
        ]))),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_has_resource_dir() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let process = ProcessRecipe {
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
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_contains_all_resources() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let fizz = brioche_test::dir(
        &brioche,
        [(
            "file.txt",
            brioche_test::file_with_resources(
                brioche_test::blob(&brioche, b"foo").await,
                false,
                brioche_test::dir_value(
                    &brioche,
                    [(
                        "foo/bar.txt",
                        brioche_test::file(
                            brioche_test::blob(&brioche, b"resource a").await,
                            false,
                        ),
                    )],
                )
                .await,
            ),
        )],
    )
    .await;

    let buzz = brioche_test::file_with_resources(
        brioche_test::blob(&brioche, b"bar").await,
        false,
        brioche_test::dir_value(
            &brioche,
            [(
                "foo/baz.txt",
                brioche_test::file(brioche_test::blob(&brioche, b"resource b").await, false),
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
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    };

    bake_without_meta(&brioche, Recipe::Process(process)).await?;

    Ok(())
}

#[tokio::test]
async fn test_bake_process_output_with_resources() -> anyhow::Result<()> {
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

    let process = ProcessRecipe {
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
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    };

    let result = bake_without_meta(&brioche, Recipe::Process(process)).await?;
    let Artifact::Directory(dir) = result else {
        panic!("expected directory");
    };

    let program = get(&brioche, &dir, "bin/program").await;
    let Artifact::File(File { resources, .. }) = &program.value else {
        panic!("expected file");
    };

    assert_eq!(
        *resources,
        brioche_test::dir_value(
            &brioche,
            [
                (
                    "program",
                    brioche_test::file(brioche_test::blob(&brioche, b"dummy_program").await, false)
                ),
                (
                    "ld-linux.so",
                    brioche_test::file(
                        brioche_test::blob(&brioche, b"dummy_ld_linux").await,
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
async fn test_bake_process_unsafe_validation() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let no_unsafety = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, no_unsafety).await, Ok(_));

    let needs_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: true,
    });

    assert_matches!(bake_without_meta(&brioche, needs_unsafe).await, Err(_));

    let unnecessary_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: true,
        networking: false,
    });

    assert_matches!(
        bake_without_meta(&brioche, unnecessary_unsafe).await,
        Err(_)
    );

    let necessary_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: true,
        networking: true,
    });

    assert_matches!(bake_without_meta(&brioche, necessary_unsafe).await, Ok(_));

    Ok(())
}

#[tokio::test]
async fn test_bake_process_networking_disabled() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Err(_));

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_process_networking_enabled() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

    let mut server = mockito::Server::new();
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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: true,
        networking: true,
    });

    assert_eq!(
        bake_without_meta(&brioche, process).await?,
        brioche_test::file(brioche_test::blob(&brioche, "hello").await, false),
    );

    hello_endpoint.assert();

    Ok(())
}

#[tokio::test]
async fn test_bake_process_networking_enabled_dns() -> anyhow::Result<()> {
    let _guard = TEST_SEMAPHORE.acquire().await.unwrap();
    let (brioche, _context) = brioche_test::brioche_test().await;

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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir_empty())),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: true,
        networking: true,
    });

    assert_matches!(bake_without_meta(&brioche, process).await, Ok(_));

    Ok(())
}
