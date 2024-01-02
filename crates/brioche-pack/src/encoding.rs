use std::{borrow::Cow, path::PathBuf};

use bstr::{ByteSlice, ByteVec as _};

pub enum UrlEncoded {}

impl<T> serde_with::SerializeAs<T> for UrlEncoded
where
    T: AsRef<[u8]>,
{
    fn serialize_as<S>(source: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let encoded = urlencoding::encode_binary(source.as_ref());
        serializer.serialize_str(encoded.as_ref())
    }
}

impl<'de, T> serde_with::DeserializeAs<'de, T> for UrlEncoded
where
    T: TryFrom<Vec<u8>>,
    T::Error: std::fmt::Display,
{
    fn deserialize_as<D>(deserializer: D) -> Result<T, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded: Cow<'de, str> = serde::de::Deserialize::deserialize(deserializer)?;
        let decoded = urlencoding::decode_binary(encoded.as_bytes());
        let deserialized = T::try_from(decoded.into_owned()).map_err(serde::de::Error::custom)?;
        Ok(deserialized)
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
