//! Serde derives reject repeated struct fields; this pass also rejects duplicate
//! keys in dynamic fact values, which Value otherwise silently overwrites.
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeSet;
use std::fmt;

struct Checked;

impl<'de> Deserialize<'de> for Checked {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(CheckedVisitor)
    }
}

struct CheckedVisitor;

impl<'de> Visitor<'de> for CheckedVisitor {
    type Value = Checked;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON with unique object keys")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Checked, E> {
        Ok(Checked)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Checked, E> {
        Ok(Checked)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Checked, E> {
        Ok(Checked)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Checked, E> {
        Ok(Checked)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Checked, E> {
        Ok(Checked)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Checked, E> {
        Ok(Checked)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Checked, A::Error> {
        while seq.next_element::<Checked>()?.is_some() {}
        Ok(Checked)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Checked, A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate object key"));
            }
            map.next_value::<Checked>()?;
        }
        Ok(Checked)
    }
}

pub(crate) fn validate(bytes: &[u8]) -> Result<(), serde_json::Error> {
    serde_json::from_slice::<Checked>(bytes).map(|_| ())
}
