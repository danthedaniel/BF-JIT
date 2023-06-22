use std::ops::Deref;

/// A container for values that can only be deref'd immutably.
#[derive(Debug, Clone)]
pub struct Immutable<T> {
    value: T,
}

impl<T> Immutable<T> {
    pub fn new(value: T) -> Self {
        Immutable { value }
    }
}

impl<T> Deref for Immutable<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: PartialEq> PartialEq<Self> for Immutable<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
