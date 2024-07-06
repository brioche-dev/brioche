use std::borrow::Cow;

pub enum TickEncoded {}

impl<T> serde_with::SerializeAs<T> for TickEncoded
where
    T: AsRef<[u8]>,
{
    fn serialize_as<S>(source: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let encoded = tick_encoding::encode(source.as_ref());
        serializer.serialize_str(encoded.as_ref())
    }
}

impl<'de, T> serde_with::DeserializeAs<'de, T> for TickEncoded
where
    T: TryFrom<Vec<u8>>,
    T::Error: std::fmt::Display,
{
    fn deserialize_as<D>(deserializer: D) -> Result<T, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded: Cow<'de, str> = serde::de::Deserialize::deserialize(deserializer)?;
        let decoded =
            tick_encoding::decode(encoded.as_bytes()).map_err(serde::de::Error::custom)?;
        let deserialized = T::try_from(decoded.into_owned()).map_err(serde::de::Error::custom)?;
        Ok(deserialized)
    }
}
