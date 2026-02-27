const HASH_LEN: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Blake3Hash([u8; HASH_LEN]);

impl Blake3Hash {
    fn to_str(self) -> Blake3HashString {
        let mut hex_bytes = [0u8; HASH_LEN * 2];
        hex::encode_to_slice(self.0, &mut hex_bytes).expect("failed to encode hash as hex");
        Blake3HashString(hex_bytes)
    }
}

impl std::fmt::Display for Blake3Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_str())
    }
}

impl std::fmt::Debug for Blake3Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_str())
    }
}

impl std::str::FromStr for Blake3Hash {
    type Err = super::ParseHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s: [u8; HASH_LEN * 2] =
            s.as_bytes()
                .try_into()
                .map_err(|_| super::ParseHashError::WrongLength {
                    expected: HASH_LEN * 2,
                    actual: s.len(),
                })?;
        let mut hash = [0u8; HASH_LEN];
        hex::decode_to_slice(s, &mut hash)?;

        Ok(Self(hash))
    }
}

impl serde::Serialize for Blake3Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_str())
    }
}

impl<'de> serde::Deserialize<'de> for Blake3Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <&str>::deserialize(deserializer)?;
        let hash: Self = s.parse().map_err(serde::de::Error::custom)?;

        Ok(hash)
    }
}

impl From<blake3::Hash> for Blake3Hash {
    fn from(value: blake3::Hash) -> Self {
        Self(*value.as_bytes())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Blake3HashString([u8; HASH_LEN * 2]);

impl std::ops::Deref for Blake3HashString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        str::from_utf8(&self.0).expect("invalid UTF-8 in Blake3HashString")
    }
}

impl std::fmt::Display for Blake3HashString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: &str = self;
        write!(f, "{s}")
    }
}
