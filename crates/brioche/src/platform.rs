#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde_with::SerializeDisplay,
    serde_with::DeserializeFromStr,
    strum::Display,
    strum::EnumString,
)]
pub enum Platform {
    #[strum(serialize = "x86_64-linux")]
    X86_64Linux,
}

cfg_if::cfg_if! {
    if #[cfg(all(target_os = "linux", target_arch = "x86_64"))] {
        pub fn current_platform() -> Platform {
            Platform::X86_64Linux
        }
    } else {
        compile_error!("unsupported platform");
    }
}
