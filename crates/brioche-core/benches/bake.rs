use brioche_core::{
    recipe::{Directory, Recipe, WithMeta},
    Brioche,
};

fn main() {
    divan::main();
}

#[divan::bench]
fn cached_bake_deep_dir(bencher: divan::Bencher) {
    let RecipesContext {
        brioche,
        runtime,
        recipes,
        ..
    } = set_up_bench();

    bencher.bench_local(|| {
        runtime.block_on(async {
            let deep_dir = WithMeta::without_meta(Recipe::from(recipes.deep_dir.clone()));
            let _ = brioche_core::bake::bake(
                &brioche,
                deep_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        });
    })
}

#[divan::bench]
fn cached_bake_wide_dir(bencher: divan::Bencher) {
    let RecipesContext {
        brioche,
        runtime,
        recipes,
        ..
    } = set_up_bench();

    bencher.bench_local(|| {
        runtime.block_on(async {
            let wide_dir = WithMeta::without_meta(Recipe::from(recipes.wide_dir.clone()));
            let _ = brioche_core::bake::bake(
                &brioche,
                wide_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        });
    })
}

#[divan::bench]
fn cached_bake_deep_merge(bencher: divan::Bencher) {
    let RecipesContext {
        brioche,
        runtime,
        recipes,
        ..
    } = set_up_bench();

    bencher.bench_local(|| {
        runtime.block_on(async {
            let merge_deep_dir = WithMeta::without_meta(recipes.merge_deep_dir.clone());
            let _ = brioche_core::bake::bake(
                &brioche,
                merge_deep_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        });
    })
}

#[divan::bench]
fn cached_bake_wide_merge(bencher: divan::Bencher) {
    let RecipesContext {
        brioche,
        runtime,
        recipes,
        ..
    } = set_up_bench();

    bencher.bench_local(|| {
        runtime.block_on(async {
            let merge_wide_dir = WithMeta::without_meta(recipes.merge_wide_dir.clone());
            let _ = brioche_core::bake::bake(
                &brioche,
                merge_wide_dir,
                &brioche_core::bake::BakeScope::Anonymous,
            )
            .await
            .unwrap();
        });
    })
}

fn set_up_bench() -> RecipesContext {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, context) = runtime.block_on(brioche_test_support::brioche_test());

    let deep_dir = runtime.block_on(make_deep_dir(&brioche, ""));
    let _deep_dir_result = runtime
        .block_on(brioche_core::bake::bake(
            &brioche,
            WithMeta::without_meta(Recipe::from(deep_dir.clone())),
            &brioche_core::bake::BakeScope::Anonymous,
        ))
        .unwrap();

    let wide_dir = runtime.block_on(make_wide_dir(&brioche, ""));
    let _wide_dir_result = runtime
        .block_on(brioche_core::bake::bake(
            &brioche,
            WithMeta::without_meta(Recipe::from(wide_dir.clone())),
            &brioche_core::bake::BakeScope::Anonymous,
        ))
        .unwrap();

    let merge_deep_dir = Recipe::Merge {
        directories: (0..10)
            .map(|n| {
                let deep_dir = runtime.block_on(make_deep_dir(&brioche, &n.to_string()));
                WithMeta::without_meta(Recipe::from(deep_dir))
            })
            .collect(),
    };

    let merge_wide_dir = Recipe::Merge {
        directories: (0..10)
            .map(|n| {
                let wide_dir = runtime.block_on(make_wide_dir(&brioche, &n.to_string()));
                WithMeta::without_meta(Recipe::from(wide_dir))
            })
            .collect(),
    };

    RecipesContext {
        brioche,
        _context: context,
        runtime,
        recipes: Recipes {
            deep_dir,
            wide_dir,
            merge_deep_dir,
            merge_wide_dir,
        },
    }
}

struct RecipesContext {
    brioche: Brioche,
    _context: brioche_test_support::TestContext,
    runtime: tokio::runtime::Runtime,
    recipes: Recipes,
}

struct Recipes {
    deep_dir: Directory,
    wide_dir: Directory,
    merge_deep_dir: Recipe,
    merge_wide_dir: Recipe,
}

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
