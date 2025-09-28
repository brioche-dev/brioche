use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;

use brioche_core::Brioche;

fn main() {
    divan::main();
}

#[divan::bench(args = [Removal::Remove, Removal::Keep])]
fn bench_input(bencher: divan::Bencher, removal: Removal) {
    bencher
        .with_inputs(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (brioche, context) = runtime.block_on(brioche_test_support::brioche_test());

            let input_root = context.path(ulid::Ulid::new().to_string());

            let input_path = input_root.join("input");
            let input_resources = input_root.join("resources");

            std::fs::create_dir_all(&input_path).unwrap();
            std::fs::create_dir_all(&input_resources).unwrap();

            for i in 0..100 {
                let mut file =
                    std::fs::File::create(input_path.join(format!("file{i}.txt"))).unwrap();

                brioche_pack::inject_pack(
                    &mut file,
                    &brioche_pack::Pack::Metadata {
                        resource_paths: vec![format!("{i}_resources").into()],
                        format: String::new(),
                        metadata: vec![],
                    },
                )
                .unwrap();

                let resource_dir = input_resources.join(format!("{i}_resources"));
                std::fs::create_dir_all(&resource_dir).unwrap();

                for j in 0..10 {
                    std::fs::write(
                        resource_dir.join(format!("resource{j}.txt")),
                        format!("{i}/{j}"),
                    )
                    .unwrap();
                }
            }

            InputContext {
                runtime,
                brioche,
                _context: context,
                input_path,
                input_resources,
            }
        })
        .bench_values(|ctx| {
            ctx.runtime.block_on(async {
                let mut saved_paths = HashMap::default();
                let meta = Arc::default();

                brioche_core::input::create_input(
                    &ctx.brioche,
                    brioche_core::input::InputOptions {
                        input_path: &ctx.input_path,
                        remove_input: removal.should_remove(),
                        resource_dir: Some(&ctx.input_resources),
                        input_resource_dirs: &[],
                        saved_paths: &mut saved_paths,
                        meta: &meta,
                    },
                )
                .await
                .unwrap();
            });
        });
}

#[divan::bench(args = [Removal::Remove, Removal::Keep])]
fn bench_input_with_shared_resources(bencher: divan::Bencher, removal: Removal) {
    bencher
        .with_inputs(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (brioche, context) = runtime.block_on(brioche_test_support::brioche_test());

            let input_root = context.path(ulid::Ulid::new().to_string());

            let input_path = input_root.join("input");
            let input_resources = input_root.join("resources");

            std::fs::create_dir_all(&input_path).unwrap();
            std::fs::create_dir_all(&input_resources).unwrap();

            for i in 0..100 {
                let mut file =
                    std::fs::File::create(input_path.join(format!("file{i}.txt"))).unwrap();

                brioche_pack::inject_pack(
                    &mut file,
                    &brioche_pack::Pack::Metadata {
                        resource_paths: vec!["common_resources".into()],
                        format: String::new(),
                        metadata: vec![],
                    },
                )
                .unwrap();
            }

            let resource_dir = input_resources.join("common_resources");
            std::fs::create_dir_all(&resource_dir).unwrap();

            for i in 0..10 {
                std::fs::write(resource_dir.join(format!("resource{i}.txt")), i.to_string())
                    .unwrap();
            }

            InputContext {
                runtime,
                brioche,
                _context: context,
                input_path,
                input_resources,
            }
        })
        .bench_values(|ctx| {
            ctx.runtime.block_on(async {
                let mut saved_paths = HashMap::default();
                let meta = Arc::default();

                brioche_core::input::create_input(
                    &ctx.brioche,
                    brioche_core::input::InputOptions {
                        input_path: &ctx.input_path,
                        remove_input: removal.should_remove(),
                        resource_dir: Some(&ctx.input_resources),
                        input_resource_dirs: &[],
                        saved_paths: &mut saved_paths,
                        meta: &meta,
                    },
                )
                .await
                .unwrap();
            });
        });
}

#[divan::bench(args = [Removal::Remove, Removal::Keep])]
fn bench_input_with_shared_ancestor_resources(bencher: divan::Bencher, removal: Removal) {
    bencher
        .with_inputs(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (brioche, context) = runtime.block_on(brioche_test_support::brioche_test());

            let input_root = context.path(ulid::Ulid::new().to_string());

            let input_path = input_root.join("input");
            let input_resources = input_root.join("resources");

            std::fs::create_dir_all(&input_path).unwrap();
            std::fs::create_dir_all(&input_resources).unwrap();

            for i in 0..100 {
                let resource_path = input_resources.join(format!("resources{i}"));
                std::fs::create_dir_all(&resource_path).unwrap();

                for j in 0..10 {
                    let inner_resource_path = resource_path.join(format!("resource{j}"));

                    let mut inner_resource_file =
                        std::fs::File::create(&inner_resource_path).unwrap();

                    writeln!(&mut inner_resource_file, "{i}/{j}").unwrap();

                    brioche_pack::inject_pack(
                        &mut inner_resource_file,
                        &brioche_pack::Pack::Metadata {
                            resource_paths: vec!["common_resources".into()],
                            format: String::new(),
                            metadata: vec![],
                        },
                    )
                    .unwrap();
                }

                let mut file =
                    std::fs::File::create(input_path.join(format!("file{i}.txt"))).unwrap();

                writeln!(&mut file, "{i}",).unwrap();

                brioche_pack::inject_pack(
                    &mut file,
                    &brioche_pack::Pack::Metadata {
                        resource_paths: vec![format!("resources{i}").into()],
                        format: String::new(),
                        metadata: vec![],
                    },
                )
                .unwrap();
            }

            std::fs::write(input_resources.join("shared.txt"), "shared").unwrap();

            let common_resource_dir = input_resources.join("common_resources");
            std::fs::create_dir_all(&common_resource_dir).unwrap();

            for i in 0..10 {
                let mut file =
                    std::fs::File::create(common_resource_dir.join(format!("resource{i}.txt")))
                        .unwrap();

                writeln!(&mut file, "resource_{i}").unwrap();

                brioche_pack::inject_pack(
                    &mut file,
                    &brioche_pack::Pack::Metadata {
                        resource_paths: vec!["shared.txt".into()],
                        format: String::new(),
                        metadata: vec![],
                    },
                )
                .unwrap();
            }

            InputContext {
                runtime,
                brioche,
                _context: context,
                input_path,
                input_resources,
            }
        })
        .bench_values(|ctx| {
            ctx.runtime.block_on(async {
                let meta = Arc::default();
                brioche_core::input::create_input(
                    &ctx.brioche,
                    brioche_core::input::InputOptions {
                        input_path: &ctx.input_path,
                        remove_input: removal.should_remove(),
                        resource_dir: Some(&ctx.input_resources),
                        input_resource_dirs: &[],
                        saved_paths: &mut HashMap::default(),
                        meta: &meta,
                    },
                )
                .await
                .unwrap();
            });
        });
}

struct InputContext {
    runtime: tokio::runtime::Runtime,
    brioche: Brioche,
    _context: brioche_test_support::TestContext,
    input_path: std::path::PathBuf,
    input_resources: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum Removal {
    Remove,
    Keep,
}

impl Removal {
    const fn should_remove(self) -> bool {
        match self {
            Self::Remove => true,
            Self::Keep => false,
        }
    }
}
