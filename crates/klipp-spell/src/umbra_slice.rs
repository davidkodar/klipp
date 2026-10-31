// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// From spellbook 0.4.2 (https://github.com/helix-editor/spellbook),
// © Michael Davis. Changed for Klipp (#3236): rewritten without `unsafe`.

//! An owned slice type with the interface of spellbook's "[Umbra strings]."
//!
//! Upstream packs the length, a prefix and either the rest of a short
//! slice or a pointer into two words, which takes `unsafe` throughout.
//! Klipp vendors spellbook without `unsafe`, so here the slice is a
//! `Box<[T]>`: the same two words on a 64-bit machine, an allocation for
//! every non-empty slice instead of none for short ones, and the same
//! interface, so the rest of the crate is unchanged. The const parameters
//! are kept for that interface and do nothing.
//!
//! [Umbra strings]: https://cedardb.com/blog/german_strings/

use core::{borrow::Borrow, fmt, hash};

use crate::alloc::{boxed::Box, string::String};

use crate::Flag;
pub type FlagSlice = UmbraSlice<Flag, 0, 0>;

/// An owned slice of at most `u16::MAX` elements.
pub struct UmbraSlice<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize>(Box<[T]>);

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[inline]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<T: Copy + Ord + Eq, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {
    /// Checks whether the given element is a member of the slice.
    ///
    /// The slice must be sorted.
    pub fn sorted_contains(&self, t: &T) -> bool {
        // `slice::contains` is ever so slightly faster than a binary search on a small slice.
        if self.0.len() <= 8 {
            self.0.contains(t)
        } else {
            self.0.binary_search(t).is_ok()
        }
    }
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Clone for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Default for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {
    fn default() -> Self {
        Self(Box::default())
    }
}

#[derive(Debug)]
pub struct TooLongError;

impl fmt::Display for TooLongError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("input slice was too long (longer than u16::MAX)")
    }
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> TryFrom<&[T]> for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {
    type Error = TooLongError;

    fn try_from(source: &[T]) -> Result<Self, Self::Error> {
        if source.len() > u16::MAX as usize {
            return Err(TooLongError);
        }
        Ok(Self(source.into()))
    }
}

impl<T: Copy + PartialEq, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> PartialEq<Self> for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T: Copy + Eq, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Eq for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {}

impl<T: Copy + fmt::Debug, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> fmt::Debug for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An owned string of at most `u16::MAX` bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct UmbraString(Box<str>);

impl UmbraString {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for UmbraString {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for UmbraString {
    fn borrow(&self) -> &str {
        self.as_ref()
    }
}

impl hash::Hash for UmbraString {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        // Note: `Hash for UmbraString` must have the same outputs as `Hash for str` in order for
        // our hash bag lookup to work.
        self.as_ref().hash(state)
    }
}

impl PartialEq<str> for UmbraString {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl fmt::Debug for UmbraString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl From<&str> for UmbraString {
    fn from(s: &str) -> Self {
        // Inputs over u16::MAX bytes are rejected at the dictionary parse boundary;
        // internal callers are guaranteed to pass bounded strings.
        assert!(s.len() <= u16::MAX as usize, "UmbraString::from called with input longer than u16::MAX bytes");
        Self(s.into())
    }
}

impl From<String> for UmbraString {
    fn from(s: String) -> Self {
        s.as_str().into()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use core::mem::size_of;

    type U16UmbraSlice = UmbraSlice<u16, 0, 0>;

    #[test]
    fn sizing() {
        assert_eq!(size_of::<U16UmbraSlice>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<UmbraString>(), 2 * size_of::<usize>());
    }

    #[test]
    fn eq() {
        assert_eq!(U16UmbraSlice::try_from([1u16].as_slice()).unwrap(), U16UmbraSlice::try_from([1u16].as_slice()).unwrap(),);
        assert_eq!(UmbraString::from(""), UmbraString::from(""));
        assert_eq!(UmbraString::from("foo"), UmbraString::from("foo"),);
    }

    #[test]
    fn partial_eq_str() {
        assert_eq!(UmbraString::from("hello"), *"hello");
        assert_eq!(UmbraString::from("foobarbaz"), *"foobarbaz");
        assert_eq!(UmbraString::from("hellohellohello"), *"hellohellohello");
    }

    #[test]
    fn clone() {
        let s = UmbraString::from("hellohellohello");
        assert_eq!(s.clone(), s);
        assert_eq!(s.clone(), *"hellohellohello");

        let s = UmbraString::from("foo");
        assert_eq!(s.clone(), s);
        assert_eq!(s.clone(), *"foo");
    }

    #[test]
    fn too_long_is_refused() {
        let long = [0u16; u16::MAX as usize + 1];
        assert!(U16UmbraSlice::try_from(long.as_slice()).is_err());
    }

    #[test]
    fn sorted_contains() {
        let short = U16UmbraSlice::try_from([1u16, 3, 5].as_slice()).unwrap();
        assert!(short.sorted_contains(&3) && !short.sorted_contains(&4));
        let long: crate::alloc::vec::Vec<u16> = (0..40).map(|n| n * 2).collect();
        let long = U16UmbraSlice::try_from(long.as_slice()).unwrap();
        assert!(long.sorted_contains(&78) && !long.sorted_contains(&79));
    }

    #[test]
    fn hash() {
        use core::hash::{BuildHasher, Hash};
        fn hash_once<T: Hash, S: BuildHasher>(hash_builder: &S, val: &T) -> u64 {
            hash_builder.hash_one(val)
        }
        let hasher = crate::DefaultHashBuilder::default();

        let s = UmbraString::from("hellohellohello");
        assert_eq!(hash_once(&hasher, &s), hash_once(&hasher, &s));
        assert_eq!(hash_once(&hasher, &s), hash_once(&hasher, &s.clone()));
        assert_eq!(hash_once(&hasher, &s), hash_once(&hasher, &UmbraString::from("hellohellohello")));
        assert_eq!(hash_once(&hasher, &s), hash_once(&hasher, &"hellohellohello"));
    }
}
