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
    #[strum(serialize = "aarch64-linux")]
    Aarch64Linux,
}

pub fn current_platform() -> Platform {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Platform::X86_64Linux
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Platform::Aarch64Linux
    } else {
        unimplemented!("unsupported platform");
    }
}
