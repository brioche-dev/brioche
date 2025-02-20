use std::{borrow::Cow, path::PathBuf};

use bstr::{ByteSlice as _, ByteVec as _};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickEncode<T>(pub T);

impl<T> serde::Serialize for TickEncode<T>
where
    T: AsRef<[u8]>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let encoded = tick_encoding::encode(self.0.as_ref());
        serializer.serialize_str(encoded.as_ref())
    }
}

impl<'de, T> serde::Deserialize<'de> for TickEncode<T>
where
    T: TryFrom<Vec<u8>>,
    T::Error: std::fmt::Display,
{
    fn deserialize<D>(deserializer: D) -> Result<TickEncode<T>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded: Cow<'de, str> = serde::de::Deserialize::deserialize(deserializer)?;
        let decoded =
            tick_encoding::decode(encoded.as_bytes()).map_err(serde::de::Error::custom)?;
        let deserialized = T::try_from(decoded.into_owned()).map_err(serde::de::Error::custom)?;
        Ok(Self(deserialized))
    }
}

pub enum TickEncoded {}

impl<T> serde_with::SerializeAs<T> for TickEncoded
where
    T: AsRef<[u8]>,
{
    fn serialize_as<S>(source: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&TickEncode(source), serializer)
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
        serde::Deserialize::deserialize(deserializer).map(|TickEncode(value)| value)
    }
}

pub struct AsPath<T>(std::marker::PhantomData<T>);

impl<T> serde_with::SerializeAs<PathBuf> for AsPath<T>
where
    T: serde_with::SerializeAs<Vec<u8>>,
{
    fn serialize_as<S>(source: &PathBuf, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = Vec::<u8>::from_path_buf(source.clone())
            .map_err(|_| serde::ser::Error::custom("invalid path"))?;
        T::serialize_as(&bytes, serializer)
    }
}

impl<'de, T> serde_with::DeserializeAs<'de, PathBuf> for AsPath<T>
where
    T: serde_with::DeserializeAs<'de, Vec<u8>>,
{
    fn deserialize_as<D>(deserializer: D) -> Result<PathBuf, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes: Vec<u8> = T::deserialize_as(deserializer)?;
        let path = bytes
            .to_path()
            .map_err(|_| serde::de::Error::custom("invalid path"))?;
        Ok(path.to_owned())
    }
}
