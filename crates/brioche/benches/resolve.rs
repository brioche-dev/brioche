use brioche::{
    artifact::{Directory, LazyArtifact, WithMeta},
    Brioche,
};
use criterion::{criterion_group, criterion_main, Criterion};
use futures::StreamExt as _;

mod brioche_bench;

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
                                Some(WithMeta::without_meta(brioche_bench::file(
                                    brioche_bench::blob(
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
                    Some(WithMeta::without_meta(brioche_bench::file(
                        brioche_bench::blob(brioche, format!("a={a},b={b}")).await,
                        false,
                    ))),
                )
                .await
                .unwrap();
        }
    }

    directory
}

fn run_resolve_benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime");
    let _runtime_guard = runtime.enter();

    struct Artifacts {
        deep_dir: Directory,
        wide_dir: Directory,
        merge_deep_dir: LazyArtifact,
        merge_wide_dir: LazyArtifact,
    }

    let (brioche, _context, artifacts) = runtime.block_on(async {
        let (brioche, context) = brioche_bench::brioche_test().await;

        let deep_dir = make_deep_dir(&brioche, "").await;
        let _deep_dir_result = brioche::resolve::resolve(
            &brioche,
            WithMeta::without_meta(LazyArtifact::from(deep_dir.clone())),
            &brioche::resolve::ResolveScope::Anonymous,
        )
        .await
        .unwrap();

        let wide_dir = make_wide_dir(&brioche, "").await;
        let _wide_dir_result = brioche::resolve::resolve(
            &brioche,
            WithMeta::without_meta(LazyArtifact::from(wide_dir.clone())),
            &brioche::resolve::ResolveScope::Anonymous,
        )
        .await
        .unwrap();

        let merge_deep_dir = LazyArtifact::Merge {
            directories: futures::stream::iter(0..10)
                .then(|n| {
                    let brioche = brioche.clone();
                    async move {
                        WithMeta::without_meta(LazyArtifact::from(
                            make_deep_dir(&brioche, &n.to_string()).await,
                        ))
                    }
                })
                .collect()
                .await,
        };

        let merge_wide_dir = LazyArtifact::Merge {
            directories: futures::stream::iter(0..10)
                .then(|n| {
                    let brioche = brioche.clone();
                    async move {
                        WithMeta::without_meta(LazyArtifact::from(
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
            Artifacts {
                deep_dir,
                wide_dir,
                merge_deep_dir,
                merge_wide_dir,
            },
        )
    });

    c.bench_function("cached resolve deep dir", |b| {
        b.to_async(&runtime).iter(|| async {
            let deep_dir = WithMeta::without_meta(LazyArtifact::from(artifacts.deep_dir.clone()));
            let _ = brioche::resolve::resolve(
                &brioche,
                deep_dir,
                &brioche::resolve::ResolveScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });

    c.bench_function("cached resolve wide dir", |b| {
        b.to_async(&runtime).iter(|| async {
            let wide_dir = WithMeta::without_meta(LazyArtifact::from(artifacts.wide_dir.clone()));
            let _ = brioche::resolve::resolve(
                &brioche,
                wide_dir,
                &brioche::resolve::ResolveScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });

    c.bench_function("cached resolve deep merge", |b| {
        b.to_async(&runtime).iter(|| async {
            let merge_deep_dir = WithMeta::without_meta(artifacts.merge_deep_dir.clone());
            let _ = brioche::resolve::resolve(
                &brioche,
                merge_deep_dir,
                &brioche::resolve::ResolveScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });

    c.bench_function("cached resolve wide merge", |b| {
        b.to_async(&runtime).iter(|| async {
            let merge_wide_dir = WithMeta::without_meta(artifacts.merge_wide_dir.clone());
            let _ = brioche::resolve::resolve(
                &brioche,
                merge_wide_dir,
                &brioche::resolve::ResolveScope::Anonymous,
            )
            .await
            .unwrap();
        })
    });
}

criterion_group!(benches, run_resolve_benchmark);
criterion_main!(benches);
