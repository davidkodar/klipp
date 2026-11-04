//! A hash of what a value serializes to, built as it serializes: no text
//! and no JSON tree, so hashing a whole project takes little memory. A
//! map's entries count in any order, since a hash map's order changes from
//! one build of it to the next.

use serde::ser::{self, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

/// Add `value`, as it serializes, to `hasher`.
pub(crate) fn hash_serialized<T: Serialize + ?Sized>(value: &T, hasher: &mut DefaultHasher) -> Result<(), HashError> {
    value.serialize(Feed(hasher))
}

#[derive(Debug)]
pub(crate) struct HashError(String);

impl std::fmt::Display for HashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HashError {}

impl ser::Error for HashError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

/// What each kind of value starts with, so that no two kinds read alike.
#[repr(u8)]
enum Tag {
    Bool,
    Int,
    Uint,
    Float,
    NotFinite,
    Char,
    Str,
    Bytes,
    None,
    Some,
    Unit,
    Variant,
    List,
    ListEnd,
    Map,
    Struct,
    StructEnd,
}

/// The serializer, writing into a hasher.
struct Feed<'a>(&'a mut DefaultHasher);

impl Feed<'_> {
    fn tag(&mut self, tag: Tag) {
        self.0.write_u8(tag as u8);
    }

    fn text(&mut self, text: &str) {
        self.0.write_usize(text.len());
        self.0.write(text.as_bytes());
    }

    /// A float by its bits, as saved text tells any two apart, except that
    /// JSON writes every value that isn't finite as null.
    fn float(&mut self, v: f64) {
        if v.is_finite() {
            self.tag(Tag::Float);
            self.0.write_u64(v.to_bits());
        } else {
            self.tag(Tag::NotFinite);
        }
    }

    fn variant(&mut self, name: &str) {
        self.tag(Tag::Variant);
        self.text(name);
    }
}

impl<'a> ser::Serializer for Feed<'a> {
    type Ok = ();
    type Error = HashError;
    type SerializeSeq = List<'a>;
    type SerializeTuple = List<'a>;
    type SerializeTupleStruct = List<'a>;
    type SerializeTupleVariant = List<'a>;
    type SerializeMap = Entries<'a>;
    type SerializeStruct = Fields<'a>;
    type SerializeStructVariant = Fields<'a>;

    fn serialize_bool(mut self, v: bool) -> Result<(), HashError> {
        self.tag(Tag::Bool);
        self.0.write_u8(v as u8);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), HashError> {
        self.serialize_i64(v as i64)
    }
    fn serialize_i16(self, v: i16) -> Result<(), HashError> {
        self.serialize_i64(v as i64)
    }
    fn serialize_i32(self, v: i32) -> Result<(), HashError> {
        self.serialize_i64(v as i64)
    }
    fn serialize_i64(mut self, v: i64) -> Result<(), HashError> {
        self.tag(Tag::Int);
        self.0.write_i64(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), HashError> {
        self.serialize_u64(v as u64)
    }
    fn serialize_u16(self, v: u16) -> Result<(), HashError> {
        self.serialize_u64(v as u64)
    }
    fn serialize_u32(self, v: u32) -> Result<(), HashError> {
        self.serialize_u64(v as u64)
    }
    fn serialize_u64(mut self, v: u64) -> Result<(), HashError> {
        self.tag(Tag::Uint);
        self.0.write_u64(v);
        Ok(())
    }
    fn serialize_f32(mut self, v: f32) -> Result<(), HashError> {
        self.float(v as f64);
        Ok(())
    }
    fn serialize_f64(mut self, v: f64) -> Result<(), HashError> {
        self.float(v);
        Ok(())
    }
    fn serialize_char(mut self, v: char) -> Result<(), HashError> {
        self.tag(Tag::Char);
        self.0.write_u32(v as u32);
        Ok(())
    }
    fn serialize_str(mut self, v: &str) -> Result<(), HashError> {
        self.tag(Tag::Str);
        self.text(v);
        Ok(())
    }
    fn serialize_bytes(mut self, v: &[u8]) -> Result<(), HashError> {
        self.tag(Tag::Bytes);
        self.0.write_usize(v.len());
        self.0.write(v);
        Ok(())
    }
    fn serialize_none(mut self) -> Result<(), HashError> {
        self.tag(Tag::None);
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(mut self, value: &T) -> Result<(), HashError> {
        self.tag(Tag::Some);
        value.serialize(self)
    }
    fn serialize_unit(mut self) -> Result<(), HashError> {
        self.tag(Tag::Unit);
        Ok(())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), HashError> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(mut self, _name: &'static str, _index: u32, variant: &'static str) -> Result<(), HashError> {
        self.variant(variant);
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, value: &T) -> Result<(), HashError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(mut self, _name: &'static str, _index: u32, variant: &'static str, value: &T) -> Result<(), HashError> {
        self.variant(variant);
        value.serialize(self)
    }
    fn serialize_seq(mut self, _len: Option<usize>) -> Result<List<'a>, HashError> {
        self.tag(Tag::List);
        Ok(List(self))
    }
    fn serialize_tuple(self, len: usize) -> Result<List<'a>, HashError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<List<'a>, HashError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(mut self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<List<'a>, HashError> {
        self.variant(variant);
        self.serialize_seq(Some(len))
    }
    fn serialize_map(mut self, _len: Option<usize>) -> Result<Entries<'a>, HashError> {
        self.tag(Tag::Map);
        Ok(Entries { out: self, sum: 0, count: 0, entry: DefaultHasher::new() })
    }
    fn serialize_struct(mut self, _name: &'static str, _len: usize) -> Result<Fields<'a>, HashError> {
        self.tag(Tag::Struct);
        Ok(Fields(self))
    }
    fn serialize_struct_variant(mut self, _name: &'static str, _index: u32, variant: &'static str, _len: usize) -> Result<Fields<'a>, HashError> {
        self.variant(variant);
        self.tag(Tag::Struct);
        Ok(Fields(self))
    }
}

/// A list, a tuple: in order.
struct List<'a>(Feed<'a>);

impl List<'_> {
    fn item<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), HashError> {
        value.serialize(Feed(&mut *self.0 .0))
    }

    fn close(mut self) -> Result<(), HashError> {
        self.0.tag(Tag::ListEnd);
        Ok(())
    }
}

macro_rules! list_of {
    ($($tr:ident :: $add:ident),*) => {$(
        impl ser::$tr for List<'_> {
            type Ok = ();
            type Error = HashError;
            fn $add<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), HashError> {
                self.item(value)
            }
            fn end(self) -> Result<(), HashError> {
                self.close()
            }
        }
    )*};
}
list_of!(SerializeSeq::serialize_element, SerializeTuple::serialize_element, SerializeTupleStruct::serialize_field, SerializeTupleVariant::serialize_field);

/// A map: each entry hashed alone and the hashes added up, so their order
/// makes no difference.
struct Entries<'a> {
    out: Feed<'a>,
    sum: u64,
    count: u64,
    entry: DefaultHasher,
}

impl ser::SerializeMap for Entries<'_> {
    type Ok = ();
    type Error = HashError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), HashError> {
        key.serialize(Feed(&mut self.entry))
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), HashError> {
        value.serialize(Feed(&mut self.entry))?;
        let entry = std::mem::take(&mut self.entry);
        self.sum = self.sum.wrapping_add(entry.finish());
        self.count += 1;
        Ok(())
    }

    fn end(self) -> Result<(), HashError> {
        self.out.0.write_u64(self.sum);
        self.out.0.write_u64(self.count);
        Ok(())
    }
}

/// A struct: its fields by name, in the order it writes them.
struct Fields<'a>(Feed<'a>);

macro_rules! fields_of {
    ($($tr:ident),*) => {$(
        impl ser::$tr for Fields<'_> {
            type Ok = ();
            type Error = HashError;
            fn serialize_field<T: Serialize + ?Sized>(&mut self, name: &'static str, value: &T) -> Result<(), HashError> {
                self.0.text(name);
                value.serialize(Feed(&mut *self.0 .0))
            }
            fn end(mut self) -> Result<(), HashError> {
                self.0.tag(Tag::StructEnd);
                Ok(())
            }
        }
    )*};
}
fields_of!(SerializeStruct, SerializeStructVariant);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap};

    fn hash<T: Serialize>(value: &T) -> u64 {
        let mut h = DefaultHasher::new();
        hash_serialized(value, &mut h).unwrap();
        h.finish()
    }

    /// The same entries in any order are the same map, at any depth, and
    /// a map built again hashes as it did.
    #[test]
    fn a_map_hashes_alike_in_any_order() {
        let entries: Vec<(String, u32)> = (0..64).map(|i| (format!("key{i}"), i)).collect();
        let forward: HashMap<_, _> = entries.iter().cloned().collect();
        let backward: HashMap<_, _> = entries.iter().rev().cloned().collect();
        assert!(forward.iter().ne(backward.iter()), "the two iterate differently, or this tests nothing");
        assert_eq!(hash(&forward), hash(&backward));
        assert_eq!(hash(&forward), hash(&entries.iter().cloned().collect::<BTreeMap<_, _>>()));
        assert_eq!(hash(&vec![Some(&forward)]), hash(&vec![Some(&backward)]));
        assert_eq!(hash(&json!({"a": 1, "b": {"c": 2, "d": 3}})), hash(&json!({"b": {"d": 3, "c": 2}, "a": 1})));
    }

    /// What tells two values apart in saved text tells their hashes apart.
    #[test]
    fn a_different_value_hashes_differently() {
        let differ = |a: serde_json::Value, b: serde_json::Value| assert_ne!(hash(&a), hash(&b), "{a} and {b}");
        differ(json!({"a": 1}), json!({"a": 2}));
        differ(json!({"a": 1}), json!({"b": 1}));
        differ(json!({"a": 1, "b": 2}), json!({"a": 2, "b": 1}));
        differ(json!({"a": 1}), json!({"a": 1, "b": 1}));
        differ(json!([1, 2]), json!([2, 1]));
        differ(json!([[1], [2]]), json!([[1, 2]]));
        differ(json!(["ab", "c"]), json!(["a", "bc"]));
        differ(json!(1), json!(1.0));
        differ(json!(null), json!(0));
        differ(json!(""), json!(null));
        assert_ne!(hash(&None::<u32>), hash(&Some(0_u32)));
        assert_ne!(hash(&0.5_f32), hash(&0.25_f32));
        assert_ne!(hash(&0.0_f64), hash(&-0.0_f64), "saved text writes them differently");
    }

    /// Fields and variants count by name, and JSON's one null stands for
    /// every float that isn't finite.
    #[test]
    fn fields_variants_and_floats_read_as_they_save() {
        #[derive(serde::Serialize)]
        struct A { x: u32, y: u32 }
        #[derive(serde::Serialize)]
        struct B { x: u32, z: u32 }
        #[derive(serde::Serialize)]
        enum E { One(u32), Two(u32), Three { a: u32 }, Four }
        assert_ne!(hash(&A { x: 1, y: 2 }), hash(&B { x: 1, z: 2 }));
        assert_ne!(hash(&E::One(1)), hash(&E::Two(1)));
        assert_ne!(hash(&E::Three { a: 1 }), hash(&E::Four));
        assert_eq!(hash(&f64::NAN), hash(&f64::INFINITY));
        assert_eq!(hash(&f32::NAN), hash(&f64::NEG_INFINITY));
    }
}
