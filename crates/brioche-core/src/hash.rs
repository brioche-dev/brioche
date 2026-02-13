const HASH_LEN: usize = blake3::OUT_LEN;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash([u8; HASH_LEN]);

impl Hash {
    fn to_str(self) -> HashString {
        let mut hex_bytes = [0u8; HASH_LEN * 2];
        hex::encode_to_slice(self.0, &mut hex_bytes).expect("failed to encode hash as hex");
        HashString(hex_bytes)
    }
}

impl std::fmt::Debug for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_str())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HashString([u8; HASH_LEN * 2]);

impl std::ops::Deref for HashString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        str::from_utf8(&self.0).expect("invalid UTF-8 in HashString")
    }
}

impl std::fmt::Display for HashString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: &str = self;
        write!(f, "{s}")
    }
}
