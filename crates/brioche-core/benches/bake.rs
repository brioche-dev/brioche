use brioche_core::{
    recipe::{Directory, Recipe, WithMeta},
    Brioche,
};
use criterion::{criterion_group, criterion_main, Criterion};
use futures::StreamExt as _;

async fn make_deep_dir(brioche: &Brioche, key: &str) -> Directory {
    let mut directory = Directory::default();
    for a in 0..10 {
        for b in 0..3 {
            for c in 0..3 {
                for d in 0..3 {
                    for e in 0..3 {
                        directory
                            .insert(
                                brioche,
                                format!(
                                    "{key}a{a}/{key}b{b}/{key}c{c}/{key}d{d}/{key}e{e}/file.txt"
                                )
                                .as_bytes(),
                                Some(WithMeta::without_meta(brioche_test_support::file(
                                    brioche_test_support::blob(
                                        brioche,
                                        format!("a={a},b={b},c={c},d={d},e={e}"),
                                    )
                                    .await,
                                    false,
                                ))),
                            )
                            .await
                            .unwrap();
                    }
                }
            }
        }
    }

    directory
}

async fn make_wide_dir(brioche: &Brioche, key: &str) -> Directory {
    let mut directory = Directory::default();
    for a in 0..100 {
        for b in 0..10 {
            directory
                .insert(
                    brioche,
                    format!("{key}a{a}/{key}b{b}/file.txt").as_bytes(),
                    Some(WithMeta::without_meta(brioche_test_support::file(
                        brioche_test_support::blob(brioche, format!("a={a},b={b}")).await,
                        false,
                    ))),
                )
                .await
                .unwrap();
        }
    }

    directory
}

fn run_bake_benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime");
    let _runtime_guard = runtime.enter();

    struct Recipes {
        deep_dir: Directory,
        wide_dir: Directory,
        merge_deep_dir: Recipe,
        merge_wide_dir: Recipe,
    }

    let (brioche, _context, recipes) = runtime.block_on(async {
        let (brioche, context) = brioche_test_support::brioche_test().await;

        let deep_dir = make_deep_dir(&brioche, "").await;
        let _deep_dir_result = brioche_core::bake::bake(
            &brioche,
            WithMeta::without_meta(Recipe::from(deep_dir.clone())),
            &brioche_core::bake::BakeScope::Anonymous,
        )
        .await
        .unwrap();

        let wide_dir = make_wide_dir(&brioche, "").await;
        let _wide_dir_result = brioche_core::bake::bake(
            &brioche,
            WithMeta::without_meta(Recipe::from(wide_dir.clone())),
            &brioche_core::bake::BakeScope::Anonymous,
        )
        .await
        .unwrap();

        let merge_deep_dir = Recipe::Merge {
            directories: futures::stream::iter(0..10)
                .then(|n| {
                    let brioche = brioche.clone();
                    async move {
                        WithMeta::without_meta(Recipe::from(
                            make_deep_dir(&brioche, &n.to_string()).await,
                        ))
                    }
                })
                .collect()
                .await,
        };

        let merge_wide_dir = Recipe::Merge {
            directories: futures::stream::iter(0..10)
                .then(|n| {
                    let brioche = brioche.clone();
                    async move {
                        WithMeta::without_meta(Recipe::from(
                            make_deep_dir(&brioche, &n.to_string()).await,
                        ))
                    }
                })
                .collect()
                .await,
        };

        (
            brioche,
            context,
            Recipes {
                deep_dir,
                wide_dir,
                merge_deep_dir,
                merge_wide_dir,
            },
        )
    });

    c.bench_function("cached bake deep dir", |b| {
        b.to_async(&runtime).iter(|| async {
            let deep_dir = WithMeta::without_meta(Recipe::from(recipes.deep_dir.clone()));
            let _ = brioche_core::bake::bake(
                &brioche,
                deep_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });

    c.bench_function("cached bake wide dir", |b| {
        b.to_async(&runtime).iter(|| async {
            let wide_dir = WithMeta::without_meta(Recipe::from(recipes.wide_dir.clone()));
            let _ = brioche_core::bake::bake(
                &brioche,
                wide_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });

    c.bench_function("cached bake deep merge", |b| {
        b.to_async(&runtime).iter(|| async {
            let merge_deep_dir = WithMeta::without_meta(recipes.merge_deep_dir.clone());
            let _ = brioche_core::bake::bake(
                &brioche,
                merge_deep_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });

    c.bench_function("cached bake wide merge", |b| {
        b.to_async(&runtime).iter(|| async {
            let merge_wide_dir = WithMeta::without_meta(recipes.merge_wide_dir.clone());
            let _ = brioche_core::bake::bake(
                &brioche,
                merge_wide_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });
}

criterion_group!(benches, run_bake_benchmark);
criterion_main!(benches);
