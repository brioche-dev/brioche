use std::borrow::Cow;

use bstr::BString;

pub trait ToTickBytes<'a> {
    type Bytes: AsRef<[u8]> + 'a;
    type Error;

    fn to_bytes(&'a self) -> Result<Self::Bytes, Self::Error>;
}

pub trait FromTickBytes: Sized {
    type Error;

    fn from_bytes(bytes: Cow<'_, [u8]>) -> Result<Self, Self::Error>;
}

impl<'a> ToTickBytes<'a> for BString {
    type Bytes = &'a [u8];

    type Error = std::convert::Infallible;

    fn to_bytes(&'a self) -> Result<Self::Bytes, Self::Error> {
        Ok(self)
    }
}

impl FromTickBytes for BString {
    type Error = std::convert::Infallible;

    fn from_bytes(bytes: Cow<'_, [u8]>) -> Result<Self, Self::Error> {
        Ok(Self::new(bytes.into_owned()))
    }
}

pub enum TickEncoded {}

impl<T> serde_with::SerializeAs<T> for TickEncoded
where
    for<'a> T: ToTickBytes<'a>,
    for<'a> <T as ToTickBytes<'a>>::Error: std::fmt::Display,
{
    fn serialize_as<S>(source: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = source.to_bytes().map_err(serde::ser::Error::custom)?;
        let encoded = tick_encoding::encode(bytes.as_ref());
        serializer.serialize_str(encoded.as_ref())
    }
}

impl<'de, T> serde_with::DeserializeAs<'de, T> for TickEncoded
where
    T: FromTickBytes,
    T::Error: std::fmt::Display,
{
    fn deserialize_as<D>(deserializer: D) -> Result<T, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded: Cow<'de, str> = serde::de::Deserialize::deserialize(deserializer)?;
        let decoded =
            tick_encoding::decode(encoded.as_bytes()).map_err(serde::de::Error::custom)?;
        let value = T::from_bytes(decoded).map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}
