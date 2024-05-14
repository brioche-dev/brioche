#![cfg_attr(all(target_os = "linux", not(test)), no_main)]

mod linux;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[cfg_attr(not(test), no_mangle)]
        #[allow(clippy::missing_safety_doc)]
        pub unsafe extern "C" fn main(argc: libc::c_int, argv: *const *const libc::c_char) -> libc::c_int {
            linux::entrypoint(argc, argv)
        }
    } else {
        fn main() {
            eprintln!("brioche-packed-userland-exec is only supported on Linux");
            std::process::exit(1);
        }
    }
}
