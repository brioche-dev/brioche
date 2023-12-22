use brioche::brioche::{value::LazyValue, Brioche};

mod brioche_test;

pub async fn resolve_to_lazy(brioche: &Brioche, value: &LazyValue) -> LazyValue {
    let resolved = brioche_test::resolve_without_meta(brioche, value.clone()).await;
    LazyValue::from(resolved.expect("failed to resolve"))
}

#[tokio::test]
async fn test_resolve_basic() -> anyhow::Result<()> {
    let (brioche, _context) = brioche_test::brioche_test().await;

    let blob_hello = brioche_test::blob(&brioche, b"hello").await;
    let file_hello = brioche_test::lazy_file(blob_hello, false);

    let empty_dir = brioche_test::lazy_dir_empty();

    let hello_dir = brioche_test::lazy_dir([
        ("hello.txt", file_hello.clone()),
        ("hi.txt", file_hello.clone()),
    ]);

    let complex_dir = brioche_test::lazy_dir([
        ("hello.txt", file_hello.clone()),
        ("hello", hello_dir.clone()),
        ("empty", empty_dir.clone()),
        ("link", brioche_test::lazy_symlink("hello.txt")),
    ]);

    assert_eq!(resolve_to_lazy(&brioche, &file_hello).await, file_hello);

    assert_eq!(resolve_to_lazy(&brioche, &empty_dir).await, empty_dir);

    assert_eq!(resolve_to_lazy(&brioche, &hello_dir).await, hello_dir);

    assert_eq!(resolve_to_lazy(&brioche, &complex_dir).await, complex_dir);

    Ok(())
}
