//! A hash of a value as it serialises, the same in every process (#1132).
//!
//! The render cache keys a clip's frames by a hash of the clip. Hashing its
//! JSON was neither stable nor total:
//! - a std `HashMap` serialises in iteration order, which std seeds per
//!   process, so the same clip hashed differently on every launch and the
//!   disk cache was never reused across restarts;
//! - JSON wants string map keys, so a clip holding a modifier on an effect
//!   parameter (`ParamId::Effect`, a struct variant) didn't serialise at all,
//!   and hashed as nothing: every edit of it had the same key.
//!
//! This serialiser writes a tagged byte stream instead. Map entries are
//! sorted by their serialised key, so the order a map was built in doesn't
//! count; keys may be any type; and every value carries a tag and every
//! string its length, so two different values can't write the same bytes.

use serde::ser::{self, Serialize};

/// The canonical bytes of `value`.
pub fn canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(4096);
    value.serialize(Ser { out: &mut out })?;
    Ok(out)
}

/// A hash of [`canonical_bytes`]: the same for equal values in every process
/// built from the same source.
pub fn canonical_hash<T: Serialize + ?Sized>(value: &T) -> Result<u64, Error> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canonical_bytes(value)?.hash(&mut h);
    Ok(h.finish())
}

/// A value whose own `Serialize` refused.
#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

// One byte before every value says what follows.
const UNIT: u8 = 0;
const BOOL: u8 = 1;
const INT: u8 = 2;
const UINT: u8 = 3;
const F32: u8 = 4;
const F64: u8 = 5;
const CHAR: u8 = 6;
const STR: u8 = 7;
const BYTES: u8 = 8;
const NONE: u8 = 9;
const SOME: u8 = 10;
const VARIANT: u8 = 11;
const NEWTYPE: u8 = 12;
const SEQ: u8 = 13;
const MAP: u8 = 14;
const STRUCT: u8 = 15;
const END: u8 = 16;

struct Ser<'a> {
    out: &'a mut Vec<u8>,
}

impl Ser<'_> {
    fn str(&mut self, s: &str) {
        self.out.push(STR);
        self.out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        self.out.extend_from_slice(s.as_bytes());
    }

    fn variant(&mut self, index: u32, name: &str) {
        self.out.push(VARIANT);
        self.out.extend_from_slice(&index.to_le_bytes());
        self.str(name);
    }

    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(Ser { out: &mut *self.out })
    }
}

/// A map: each entry serialised on its own, then written sorted by key.
struct MapSer<'a> {
    out: &'a mut Vec<u8>,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    key: Option<Vec<u8>>,
}

impl<'a> ser::Serializer for Ser<'a> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Ser<'a>;
    type SerializeTuple = Ser<'a>;
    type SerializeTupleStruct = Ser<'a>;
    type SerializeTupleVariant = Ser<'a>;
    type SerializeMap = MapSer<'a>;
    type SerializeStruct = Ser<'a>;
    type SerializeStructVariant = Ser<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), Error> {
        self.out.extend_from_slice(&[BOOL, u8::from(v)]);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Error> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<(), Error> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<(), Error> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<(), Error> {
        self.out.push(INT);
        self.out.extend_from_slice(&i128::from(v).to_le_bytes());
        Ok(())
    }
    fn serialize_i128(self, v: i128) -> Result<(), Error> {
        self.out.push(INT);
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Error> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<(), Error> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<(), Error> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<(), Error> {
        self.out.push(UINT);
        self.out.extend_from_slice(&u128::from(v).to_le_bytes());
        Ok(())
    }
    fn serialize_u128(self, v: u128) -> Result<(), Error> {
        self.out.push(UINT);
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), Error> {
        self.out.push(F32);
        self.out.extend_from_slice(&v.to_bits().to_le_bytes());
        Ok(())
    }
    fn serialize_f64(self, v: f64) -> Result<(), Error> {
        self.out.push(F64);
        self.out.extend_from_slice(&v.to_bits().to_le_bytes());
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), Error> {
        self.out.push(CHAR);
        self.out.extend_from_slice(&u32::from(v).to_le_bytes());
        Ok(())
    }
    fn serialize_str(mut self, v: &str) -> Result<(), Error> {
        self.str(v);
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        self.out.push(BYTES);
        self.out.extend_from_slice(&(v.len() as u64).to_le_bytes());
        self.out.extend_from_slice(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Error> {
        self.out.push(NONE);
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(mut self, value: &T) -> Result<(), Error> {
        self.out.push(SOME);
        self.element(value)
    }
    fn serialize_unit(self) -> Result<(), Error> {
        self.out.push(UNIT);
        Ok(())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(mut self, _name: &'static str, index: u32, variant: &'static str) -> Result<(), Error> {
        self.variant(index, variant);
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(mut self, _name: &'static str, value: &T) -> Result<(), Error> {
        self.out.push(NEWTYPE);
        self.element(value)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(mut self, _name: &'static str, index: u32, variant: &'static str, value: &T) -> Result<(), Error> {
        self.variant(index, variant);
        self.element(value)
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Ser<'a>, Error> {
        self.out.push(SEQ);
        Ok(self)
    }
    fn serialize_tuple(self, len: usize) -> Result<Ser<'a>, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Ser<'a>, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(mut self, _name: &'static str, index: u32, variant: &'static str, _len: usize) -> Result<Ser<'a>, Error> {
        self.variant(index, variant);
        self.out.push(SEQ);
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<MapSer<'a>, Error> {
        Ok(MapSer { out: self.out, entries: Vec::with_capacity(len.unwrap_or(0)), key: None })
    }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Ser<'a>, Error> {
        self.out.push(STRUCT);
        Ok(self)
    }
    fn serialize_struct_variant(mut self, _name: &'static str, index: u32, variant: &'static str, _len: usize) -> Result<Ser<'a>, Error> {
        self.variant(index, variant);
        self.out.push(STRUCT);
        Ok(self)
    }
}

impl ser::SerializeSeq for Ser<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.out.push(END);
        Ok(())
    }
}

impl ser::SerializeTuple for Ser<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.out.push(END);
        Ok(())
    }
}

impl ser::SerializeTupleStruct for Ser<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.out.push(END);
        Ok(())
    }
}

impl ser::SerializeTupleVariant for Ser<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.out.push(END);
        Ok(())
    }
}

impl ser::SerializeStruct for Ser<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Error> {
        self.str(key);
        self.element(value)
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), Error> {
        // A skipped field and a present one must not hash alike.
        self.str(key);
        self.out.push(UNIT);
        Ok(())
    }
    fn end(self) -> Result<(), Error> {
        self.out.push(END);
        Ok(())
    }
}

impl ser::SerializeStructVariant for Ser<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Error> {
        self.str(key);
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.out.push(END);
        Ok(())
    }
}

impl ser::SerializeMap for MapSer<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.key = Some(canonical_bytes(key)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        let key = self.key.take().ok_or_else(|| Error("a map value without its key".into()))?;
        self.entries.push((key, canonical_bytes(value)?));
        Ok(())
    }
    fn end(mut self) -> Result<(), Error> {
        self.entries.sort_unstable();
        self.out.push(MAP);
        self.out.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
        for (key, value) in &self.entries {
            self.out.extend_from_slice(key);
            self.out.extend_from_slice(value);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::hash::RandomState;

    /// Two maps with the same entries, built with different hash seeds as
    /// two processes would: their JSON differs, their canonical bytes don't.
    #[test]
    fn a_map_hashes_the_same_whatever_order_it_iterates_in() {
        let entries: Vec<(String, u32)> = (0..32).map(|i| (format!("key{i}"), i)).collect();
        let mut a: HashMap<String, u32, RandomState> = HashMap::with_hasher(RandomState::new());
        let mut b: HashMap<String, u32, RandomState> = HashMap::with_hasher(RandomState::new());
        a.extend(entries.iter().cloned());
        b.extend(entries.iter().rev().cloned());
        assert_ne!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap(), "the premise: JSON follows iteration order");
        assert_eq!(canonical_bytes(&a).unwrap(), canonical_bytes(&b).unwrap());
        b.insert("key0".into(), 99);
        assert_ne!(canonical_hash(&a).unwrap(), canonical_hash(&b).unwrap(), "a changed value changes the hash");
    }

    /// A clip whose node positions and modifiers were built in two
    /// different orders has one key; a clip with a modifier on an effect
    /// parameter, which JSON refuses as a map key, hashes, and a change to
    /// its grade changes the key.
    #[test]
    fn a_clip_hashes_alike_across_orders_and_with_any_modifier_key() {
        use klipp_core::{MediaId, ParamId, RationalTime};
        use klipp_timeline::GradeAccess;
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        let base = klipp_timeline::Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let ids: Vec<klipp_nodes::NodeId> = (1..=16u128).map(uuid::Uuid::from_u128).collect();
        let params = [ParamId::Opacity, ParamId::Scale, ParamId::Effect { effect_index: 0, param_name: "radius".into() }];
        let build = |forward: bool| {
            let mut c = base.clone();
            let mut order: Vec<usize> = (0..ids.len()).collect();
            if !forward {
                order.reverse();
            }
            c.edit_grade(|g| {
                g.node_positions = HashMap::with_hasher(Default::default());
                for &i in &order {
                    g.node_positions.insert(ids[i], klipp_nodes::NodePos { x: i as f32, y: 0.0 });
                }
            });
            // A node frame with several members: a set, hashed in an
            // order that doesn't depend on how it was built.
            let mut members: Vec<klipp_nodes::NodeId> = ids[..6].to_vec();
            if !forward {
                members.reverse();
            }
            c.edit_grade(|g| {
                g.frames = vec![klipp_nodes::NodeFrame {
                    id: uuid::Uuid::from_u128(99),
                    label: "group".into(),
                    description: String::new(),
                    color_rgba: 0,
                    contained_node_ids: members.into_iter().collect(),
                    recipe: None,
                }];
            });
            let mut p: Vec<&ParamId> = params.iter().collect();
            if !forward {
                p.reverse();
            }
            for id in p {
                c.edit_param_modifiers(|v| v.insert(id.clone(), Default::default()));
            }
            c
        };
        let (a, b) = (build(true), build(false));
        assert!(serde_json::to_vec(&a).is_err(), "the premise: JSON refuses ParamId::Effect as a key");
        assert_eq!(canonical_hash(&a).unwrap(), canonical_hash(&b).unwrap());
        let mut graded = a.clone();
        graded.edit_grade(|g| {
            let c = g.ensure_corrector();
            g.nodes.iter_mut().find(|n| n.id == c).unwrap().params.exposure = Some(0.4);
        });
        assert_ne!(graded.grade(), a.grade(), "the premise: the grade changed");
        assert_ne!(canonical_hash(&a).unwrap(), canonical_hash(&graded).unwrap(), "a grade change changes the key");
    }
}
