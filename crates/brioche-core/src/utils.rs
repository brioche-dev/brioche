pub fn is_default<T>(value: &T) -> bool
where
    T: Default + PartialEq,
{
    *value == Default::default()
}
