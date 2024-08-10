use brioche_core::blob::{get_save_blob_permit, save_blob, save_blob_from_reader, SaveBlobOptions};

fn main() {
    divan::main();
}

#[divan::bench(args = [1, 10, 100])]
fn bench_blob_save_cached(bencher: divan::Bencher, num_blobs: u32) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, _context) = runtime.block_on(brioche_test_support::brioche_test());

    runtime.block_on(async {
        let mut permit = get_save_blob_permit().await.unwrap();

        for i in 0..num_blobs {
            let bytes = i.to_be_bytes();
            save_blob(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                .await
                .unwrap();
        }
    });

    bencher.bench_local(|| {
        runtime.block_on(async {
            let mut permit = get_save_blob_permit().await.unwrap();
            for i in 0..num_blobs {
                let bytes = i.to_be_bytes();
                save_blob(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                    .await
                    .unwrap();
            }
        });
    });
}

#[divan::bench(args = [1, 10, 100])]
fn bench_blob_save_uncached(bencher: divan::Bencher, num_blobs: u32) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, _context) = runtime.block_on(brioche_test_support::brioche_test());

    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    bencher.bench_local(|| {
        runtime.block_on(async {
            let mut permit = get_save_blob_permit().await.unwrap();
            for _ in 0..num_blobs {
                let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let bytes = n.to_be_bytes();
                save_blob(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                    .await
                    .unwrap();
            }
        });
    });
}

#[divan::bench(args = [1, 10, 100])]
fn bench_blob_save_reader_cached(bencher: divan::Bencher, num_blobs: u32) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, _context) = runtime.block_on(brioche_test_support::brioche_test());

    runtime.block_on(async {
        let mut permit = get_save_blob_permit().await.unwrap();

        for i in 0..num_blobs {
            let bytes = i.to_be_bytes();
            save_blob_from_reader(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                .await
                .unwrap();
        }
    });

    bencher.bench_local(|| {
        runtime.block_on(async {
            let mut permit = get_save_blob_permit().await.unwrap();
            for i in 0..num_blobs {
                let bytes = i.to_be_bytes();
                save_blob(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                    .await
                    .unwrap();
            }
        });
    });
}

#[divan::bench(args = [1, 10, 100])]
fn bench_blob_save_reader_uncached(bencher: divan::Bencher, num_blobs: u32) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, _context) = runtime.block_on(brioche_test_support::brioche_test());

    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    bencher.bench_local(|| {
        runtime.block_on(async {
            let mut permit = get_save_blob_permit().await.unwrap();
            for _ in 0..num_blobs {
                let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let bytes = n.to_be_bytes();
                save_blob_from_reader(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                    .await
                    .unwrap();
            }
        });
    });
}

#[divan::bench(args = [1, 10, 100])]
fn bench_blob_save_file_cached(bencher: divan::Bencher, num_blobs: u32) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, _context) = runtime.block_on(brioche_test_support::brioche_test());

    runtime.block_on(async {
        let mut permit = get_save_blob_permit().await.unwrap();

        for i in 0..num_blobs {
            let bytes = i.to_be_bytes();
            save_blob_from_reader(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                .await
                .unwrap();
        }
    });

    bencher.bench_local(|| {
        runtime.block_on(async {
            let mut permit = get_save_blob_permit().await.unwrap();
            for i in 0..num_blobs {
                let bytes = i.to_be_bytes();
                save_blob(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                    .await
                    .unwrap();
            }
        });
    });
}

#[divan::bench(args = [1, 10, 100])]
fn bench_blob_save_file_uncached(bencher: divan::Bencher, num_blobs: u32) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, _context) = runtime.block_on(brioche_test_support::brioche_test());

    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    bencher.bench_local(|| {
        runtime.block_on(async {
            let mut permit = get_save_blob_permit().await.unwrap();
            for _ in 0..num_blobs {
                let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let bytes = n.to_be_bytes();
                save_blob_from_reader(&brioche, &mut permit, &bytes[..], SaveBlobOptions::new())
                    .await
                    .unwrap();
            }
        });
    });
}
