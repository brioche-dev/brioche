use brioche_core::recipe::Directory;

fn main() {
    divan::main();
}

#[divan::bench]
fn bench_directory_insert(bencher: divan::Bencher) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (brioche, _context) = runtime.block_on(brioche_test_support::brioche_test());

    let blob_hello = runtime.block_on(brioche_test_support::blob(&brioche, b"hello"));
    let file_hello = brioche_test_support::file(blob_hello, false);

    bencher.bench_local(|| {
        let mut directory = Directory::default();

        runtime.block_on(async {
            for a in 0..10 {
                for b in 0..3 {
                    for c in 0..3 {
                        for d in 0..3 {
                            for e in 0..3 {
                                let path = format!("a{a}/b{b}/c{c}/d{d}/e{e}/file.txt");
                                directory
                                    .insert(&brioche, path.as_bytes(), Some(file_hello.clone()))
                                    .await
                                    .unwrap();
                            }
                        }
                    }
                }
            }
        });
    });
}
