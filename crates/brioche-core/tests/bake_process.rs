#![cfg(target_os = "linux")]

use std::collections::BTreeMap;

use anyhow::Context;
use assert_matches::assert_matches;
use pretty_assertions::assert_eq;

use brioche_core::{
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

fn home_dir() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::HomeDir],
    }
}

fn resource_dir() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::ResourceDir],
    }
}

fn input_resource_dirs() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::InputResourceDirs],
    }
}

fn work_dir() -> ProcessTemplate {
    ProcessTemplate {
        components: vec![ProcessTemplateComponent::WorkDir],
    }
}

fn temp_dir() -> ProcessTemplate {
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
    brioche: &brioche_core::Brioche,
    dir: &Directory,
    path: impl AsRef<[u8]>,
) -> anyhow::Result<Option<WithMeta<Artifact>>> {
    let artifact = dir.get(brioche, path.as_ref()).await?;
    Ok(artifact)
}

async fn get(
    brioche: &brioche_core::Brioche,
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

fn default_process() -> ProcessRecipe {
    ProcessRecipe {
        command: ProcessTemplate { components: vec![] },
        args: vec![],
        env: BTreeMap::new(),
        dependencies: vec![],
        work_dir: Box::new(WithMeta::without_meta(Recipe::Directory(
            Directory::default(),
        ))),
        output_scaffold: None,
        platform: current_platform(),
        is_unsafe: false,
        networking: false,
    }
}

macro_rules! run_test {
    ($brioche_test:expr, $test:ident) => {
        (
            stringify!($test),
            $test(&($brioche_test).0, &($brioche_test).1).await,
        )
    };
}

// NOTE: Rather than having each test case be a separate test, we use one
// test to run lots of test cases. This is for two reasons: first, there's some
// issue when running multiple processes that causes a deadlock (this would be
// solvable with a semaphore / mutex, but wouldn't be any faster). Second, many
// of these tests need to access resources from the internet, and in separate
// tests these would need to be downloaded separately. Using one test with
// test cases that all share one `brioche_core::Brioche` instance speeds up the
// test cases drastically.
#[tokio::test]
async fn test_bake_process() -> anyhow::Result<()> {
    let brioche_test = brioche_test::brioche_test().await;
    let results = [
        run_test!(brioche_test, test_bake_process_simple),
        run_test!(brioche_test, test_bake_process_fail_on_no_output),
        run_test!(brioche_test, test_bake_process_scaffold_output),
        run_test!(brioche_test, test_bake_process_scaffold_and_modify_output),
        run_test!(brioche_test, test_bake_process_fail_on_non_zero_exit),
        run_test!(brioche_test, test_bake_process_command_no_path),
        run_test!(brioche_test, test_bake_process_command_path),
        run_test!(brioche_test, test_bake_process_with_utils),
        run_test!(brioche_test, test_bake_process_with_readonly_contents),
        run_test!(brioche_test, test_bake_process_cached),
        run_test!(brioche_test, test_bake_process_cached_equivalent_inputs),
        run_test!(
            brioche_test,
            test_bake_process_cached_equivalent_inputs_parallel
        ),
        run_test!(brioche_test, test_bake_process_cache_busted),
        run_test!(brioche_test, test_bake_process_custom_env_vars),
        run_test!(brioche_test, test_bake_process_no_default_env_vars),
        run_test!(brioche_test, test_bake_process_no_default_path),
        run_test!(brioche_test, test_bake_process_command_ambiguous_error),
        run_test!(brioche_test, test_bake_process_command_uses_path),
        run_test!(brioche_test, test_bake_process_command_uses_dependencies),
        run_test!(
            brioche_test,
            test_bake_process_starts_with_work_dir_contents
        ),
        run_test!(brioche_test, test_bake_process_edit_work_dir_contents),
        run_test!(brioche_test, test_bake_process_has_resource_dir),
        run_test!(brioche_test, test_bake_process_contains_all_resources),
        run_test!(brioche_test, test_bake_process_output_with_resources),
        run_test!(brioche_test, test_bake_process_unsafe_validation),
        run_test!(brioche_test, test_bake_process_networking_disabled),
        run_test!(brioche_test, test_bake_process_networking_enabled),
        run_test!(brioche_test, test_bake_process_networking_enabled_dns),
        run_test!(brioche_test, test_bake_process_dependencies),
    ];

    let mut failures = 0;
    for (name, result) in results {
        match result {
            Ok(_) => println!("{}: ok", name),
            Err(err) => {
                println!("{}: failed: {:#}", name, err);
                failures += 1;
            }
        }
    }

    if failures > 0 {
        anyhow::bail!(
            "{failures} test{s} failed",
            s = if failures == 1 { "" } else { "s" }
        );
    }

    Ok(())
}

async fn test_bake_process_simple(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let hello = "hello";
    let hello_blob = brioche_test::blob(brioche, hello).await;

    let hello_process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(brioche, hello_process).await?,
        brioche_test::file(hello_blob, false),
    );

    Ok(())
}

async fn test_bake_process_fail_on_no_output(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("# ... doing nothing ...")],
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, process).await, Err(_));

    Ok(())
}

async fn test_bake_process_scaffold_output(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let hello_blob = brioche_test::blob(brioche, "hello").await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("# ... doing nothing ...")],
        output_scaffold: Some(Box::new(WithMeta::without_meta(brioche_test::lazy_file(
            hello_blob, false,
        )))),
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(brioche, process).await?,
        brioche_test::file(hello_blob, false)
    );

    Ok(())
}

async fn test_bake_process_scaffold_and_modify_output(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let hello_blob = brioche_test::blob(brioche, "hello").await;

    let process = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![
            tpl("sh"),
            tpl("-c"),
            tpl(r#"echo -n hello > "$BRIOCHE_OUTPUT/hi.txt""#),
        ],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        output_scaffold: Some(Box::new(WithMeta::without_meta(
            brioche_test::lazy_dir_empty(),
        ))),
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(brioche, process).await?,
        brioche_test::dir(
            brioche,
            [("hi.txt", brioche_test::file(hello_blob, false)),]
        )
        .await
    );

    Ok(())
}

async fn test_bake_process_fail_on_non_zero_exit(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    assert_matches!(bake_without_meta(brioche, process).await, Err(_));

    Ok(())
}

async fn test_bake_process_command_no_path(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let process = Recipe::Process(ProcessRecipe {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, process).await, Err(_));

    Ok(())
}

// For now, the `command` for a process is not resolved using `$PATH`. This
// is how `unshare` works, since it uses `execve` to run the command rather
// than `execvpe`
async fn test_bake_process_command_path(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let process = Recipe::Process(ProcessRecipe {
        command: tpl("env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([
            ("BRIOCHE_OUTPUT".into(), output_path()),
            ("PATH".into(), tpl("/usr/bin")),
        ]),
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, process).await, Err(_));

    Ok(())
}

async fn test_bake_process_with_utils(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    assert_matches!(bake_without_meta(brioche, process).await, Ok(_));

    Ok(())
}

async fn test_bake_process_with_readonly_contents(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    assert_matches!(bake_without_meta(brioche, process).await, Ok(_));

    Ok(())
}

async fn test_bake_process_cached(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    let random_1 = bake_without_meta(brioche, process_random.clone()).await?;
    let random_2 = bake_without_meta(brioche, process_random).await?;

    assert_eq!(random_1, random_2);

    Ok(())
}

async fn test_bake_process_cached_equivalent_inputs(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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
        ..default_process()
    });

    let empty_dir_1_baked = bake_without_meta(brioche, empty_dir_1.clone()).await?;
    let empty_dir_2_baked = bake_without_meta(brioche, empty_dir_2.clone()).await?;
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

    let random_1 = bake_without_meta(brioche, process_random_1).await?;
    let random_2 = bake_without_meta(brioche, process_random_2).await?;

    assert_eq!(random_1, random_2);

    Ok(())
}

async fn test_bake_process_cached_equivalent_inputs_parallel(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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
        ..default_process()
    });

    let empty_dir_1_baked = bake_without_meta(brioche, empty_dir_1.clone()).await?;
    let empty_dir_2_baked = bake_without_meta(brioche, empty_dir_2.clone()).await?;
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
        brioche_core::bake::create_proxy(brioche, process_random_1.clone()).await?;
    let process_random_2_proxy =
        brioche_core::bake::create_proxy(brioche, process_random_2.clone()).await?;
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

    let baked = bake_without_meta(brioche, merged).await?;
    let Artifact::Directory(baked) = baked else {
        panic!("expected directory");
    };

    let baked_a1 = get(brioche, &baked, "a/process1.txt").await;
    let baked_a1p = get(brioche, &baked, "a/process1p.txt").await;
    let baked_b1 = get(brioche, &baked, "b/process1.txt").await;
    let baked_b1p = get(brioche, &baked, "b/process1p.txt").await;

    let baked_a2 = get(brioche, &baked, "a/process2.txt").await;
    let baked_a2p = get(brioche, &baked, "a/process2p.txt").await;
    let baked_b2 = get(brioche, &baked, "b/process2.txt").await;
    let baked_b2p = get(brioche, &baked, "b/process2p.txt").await;

    assert_eq!(baked_a1, baked_a1p);
    assert_eq!(baked_a1, baked_b1);
    assert_eq!(baked_a1, baked_b1p);

    assert_eq!(baked_a2, baked_a2p);
    assert_eq!(baked_a2, baked_b2);
    assert_eq!(baked_a2, baked_b2p);

    Ok(())
}

async fn test_bake_process_cache_busted(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    let random_1 = bake_without_meta(brioche, Recipe::Process(process_random_1)).await?;
    let random_2 = bake_without_meta(brioche, Recipe::Process(process_random_2)).await?;

    assert_ne!(random_1, random_2);

    Ok(())
}

async fn test_bake_process_custom_env_vars(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_no_default_env_vars(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_no_default_path(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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
        bake_without_meta(brioche, Recipe::Process(process)).await,
        Err(_)
    );

    Ok(())
}

async fn test_bake_process_command_ambiguous_error(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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
        bake_without_meta(brioche, Recipe::Process(process)).await,
        Err(_)
    );

    Ok(())
}

async fn test_bake_process_command_uses_path(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_command_uses_dependencies(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_starts_with_work_dir_contents(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let hello_blob = brioche_test::blob(brioche, b"hello").await;

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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir([(
            "file.txt",
            brioche_test::lazy_file(hello_blob, false),
        )]))),
        ..default_process()
    };

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_edit_work_dir_contents(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let hello_blob = brioche_test::blob(brioche, b"hello").await;

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
        work_dir: Box::new(brioche_test::without_meta(brioche_test::lazy_dir([
            ("file.txt", brioche_test::lazy_file(hello_blob, false)),
            (
                "dir",
                brioche_test::lazy_dir([("other.txt", brioche_test::lazy_file(hello_blob, false))]),
            ),
        ]))),
        ..default_process()
    };

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_has_resource_dir(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_contains_all_resources(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let fizz = brioche_test::dir(
        brioche,
        [(
            "file.txt",
            brioche_test::file_with_resources(
                brioche_test::blob(brioche, b"foo").await,
                false,
                brioche_test::dir_value(
                    brioche,
                    [(
                        "foo/bar.txt",
                        brioche_test::file(brioche_test::blob(brioche, b"resource a").await, false),
                    )],
                )
                .await,
            ),
        )],
    )
    .await;

    let buzz = brioche_test::file_with_resources(
        brioche_test::blob(brioche, b"bar").await,
        false,
        brioche_test::dir_value(
            brioche,
            [(
                "foo/baz.txt",
                brioche_test::file(brioche_test::blob(brioche, b"resource b").await, false),
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

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}

async fn test_bake_process_output_with_resources(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    let dummy_packed_blob = brioche_test::blob(brioche, &dummy_packed_contents).await;
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

    let result = bake_without_meta(brioche, Recipe::Process(process)).await?;
    let Artifact::Directory(dir) = result else {
        panic!("expected directory");
    };

    let program = get(brioche, &dir, "bin/program").await;
    let Artifact::File(File { resources, .. }) = &program.value else {
        panic!("expected file");
    };

    assert_eq!(
        *resources,
        brioche_test::dir_value(
            brioche,
            [
                (
                    "program",
                    brioche_test::file(brioche_test::blob(brioche, b"dummy_program").await, false)
                ),
                (
                    "ld-linux.so",
                    brioche_test::file(brioche_test::blob(brioche, b"dummy_ld_linux").await, false)
                ),
            ]
        )
        .await
    );

    Ok(())
}

async fn test_bake_process_unsafe_validation(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let no_unsafety = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        platform: current_platform(),
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, no_unsafety).await, Ok(_));

    let needs_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        is_unsafe: false,
        networking: true,
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, needs_unsafe).await, Err(_));

    let unnecessary_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        is_unsafe: true,
        networking: false,
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, unnecessary_unsafe).await, Err(_));

    let necessary_unsafe = Recipe::Process(ProcessRecipe {
        command: tpl("/usr/bin/env"),
        args: vec![tpl("sh"), tpl("-c"), tpl("echo -n hello > $BRIOCHE_OUTPUT")],
        env: BTreeMap::from_iter([("BRIOCHE_OUTPUT".into(), output_path())]),
        is_unsafe: true,
        networking: true,
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, necessary_unsafe).await, Ok(_));

    Ok(())
}

async fn test_bake_process_networking_disabled(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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
        is_unsafe: false,
        networking: false,
        ..default_process()
    });

    assert_matches!(bake_without_meta(brioche, process).await, Err(_));

    hello_endpoint.assert();

    Ok(())
}

async fn test_bake_process_networking_enabled(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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
        is_unsafe: true,
        networking: true,
        ..default_process()
    });

    assert_eq!(
        bake_without_meta(brioche, process).await?,
        brioche_test::file(brioche_test::blob(brioche, "hello").await, false),
    );

    hello_endpoint.assert();

    Ok(())
}

async fn test_bake_process_networking_enabled_dns(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
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

    assert_matches!(bake_without_meta(brioche, process).await, Ok(_));

    Ok(())
}

async fn test_bake_process_dependencies(
    brioche: &brioche_core::Brioche,
    _context: &brioche_test::TestContext,
) -> anyhow::Result<()> {
    let dep1 = brioche_test::dir(
        brioche,
        [
            ("a", brioche_test::dir_empty()),
            ("b", brioche_test::dir_empty()),
            ("c", brioche_test::dir_empty()),
            ("bin", brioche_test::dir_empty()),
            (
                "brioche-env.d",
                brioche_test::dir(
                    brioche,
                    [(
                        "env",
                        brioche_test::dir(
                            brioche,
                            [
                                (
                                    "PATH",
                                    brioche_test::dir(
                                        brioche,
                                        [
                                            ("a", brioche_test::symlink(b"../../../a")),
                                            ("b", brioche_test::symlink(b"../../../b")),
                                        ],
                                    )
                                    .await,
                                ),
                                (
                                    "DEP1",
                                    brioche_test::dir(
                                        brioche,
                                        [("a", brioche_test::symlink(b"../../../c"))],
                                    )
                                    .await,
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
    let dep2 = brioche_test::dir(
        brioche,
        [
            ("d", brioche_test::dir_empty()),
            ("e", brioche_test::dir_empty()),
            ("f", brioche_test::dir_empty()),
            ("bin", brioche_test::dir_empty()),
            (
                "brioche-env.d",
                brioche_test::dir(
                    brioche,
                    [(
                        "env",
                        brioche_test::dir(
                            brioche,
                            [
                                (
                                    "PATH",
                                    brioche_test::dir(
                                        brioche,
                                        [
                                            ("d", brioche_test::symlink(b"../../../d")),
                                            ("e", brioche_test::symlink(b"../../../e")),
                                        ],
                                    )
                                    .await,
                                ),
                                (
                                    "DEP2",
                                    brioche_test::dir(
                                        brioche,
                                        [("a", brioche_test::symlink(b"../../../f"))],
                                    )
                                    .await,
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
                "EXPECTED_DEP2".into(),
                tpl_join([template_input(dep2.clone().into()), tpl("/f")]),
            ),
        ]),
        dependencies: vec![
            WithMeta::without_meta(dep1.clone().into()),
            WithMeta::without_meta(dep2.clone().into()),
        ],
        ..default_process()
    };

    bake_without_meta(brioche, Recipe::Process(process)).await?;

    Ok(())
}
