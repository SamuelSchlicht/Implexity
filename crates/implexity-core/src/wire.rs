// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde::Serialize;
use serde::ser;
use serde_json::Value;

use crate::error::{CaeError, CaeResult};
use crate::json::{DumpOptions, sha256_of};

const NON_FINITE: &str = "non-finite number in CAE wire document";


pub fn to_wire<T: Serialize + ?Sized>(value: &T) -> CaeResult<Value> {
    ensure_finite(value)?;
    serde_json::to_value(value).map_err(|e| {
        let text = e.to_string();
        if text.contains("key must be a string") {
            CaeError::contract("CAE wire mapping keys must be strings")
        } else {
            CaeError::contract(format!("unsupported CAE wire type: {text}"))
        }
    })
}


pub fn ensure_finite<T: Serialize + ?Sized>(value: &T) -> CaeResult<()> {
    value.serialize(FiniteCheck).map_err(|e| CaeError::contract(e.0))
}


pub fn finite(value: f64) -> CaeResult<f64> {
    if value.is_finite() { Ok(value) } else { Err(CaeError::contract(NON_FINITE)) }
}


pub fn float_value(value: f64) -> CaeResult<Value> {
    serde_json::Number::from_f64(value).map(Value::Number).ok_or_else(|| CaeError::contract(NON_FINITE))
}


pub fn fingerprint<T: Serialize + ?Sized>(value: &T) -> CaeResult<String> {
    Ok(sha256_of(&to_wire(value)?, &DumpOptions::canonical()))
}

#[must_use]
pub fn fingerprint_value(value: &Value) -> String {
    sha256_of(value, &DumpOptions::canonical())
}

#[derive(Debug)]
struct CheckError(String);

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CheckError {}

impl ser::Error for CheckError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

struct FiniteCheck;

type R = Result<(), CheckError>;

impl ser::Serializer for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, _: bool) -> R {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> R {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> R {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> R {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> R {
        Ok(())
    }
    fn serialize_i128(self, _: i128) -> R {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> R {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> R {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> R {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> R {
        Ok(())
    }
    fn serialize_u128(self, _: u128) -> R {
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> R {
        if v.is_finite() { Ok(()) } else { Err(CheckError(NON_FINITE.into())) }
    }
    fn serialize_f64(self, v: f64) -> R {
        if v.is_finite() { Ok(()) } else { Err(CheckError(NON_FINITE.into())) }
    }
    fn serialize_char(self, _: char) -> R {
        Ok(())
    }
    fn serialize_str(self, _: &str) -> R {
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> R {
        Ok(())
    }
    fn serialize_none(self) -> R {
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> R {
        value.serialize(self)
    }
    fn serialize_unit(self) -> R {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> R {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> R {
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, value: &T) -> R {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        value: &T,
    ) -> R {
        value.serialize(self)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, CheckError> {
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, CheckError> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, CheckError> {
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, CheckError> {
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, CheckError> {
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, CheckError> {
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, CheckError> {
        Ok(self)
    }
}

impl ser::SerializeSeq for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> R {
        value.serialize(FiniteCheck)
    }
    fn end(self) -> R {
        Ok(())
    }
}

impl ser::SerializeTuple for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> R {
        value.serialize(FiniteCheck)
    }
    fn end(self) -> R {
        Ok(())
    }
}

impl ser::SerializeTupleStruct for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> R {
        value.serialize(FiniteCheck)
    }
    fn end(self) -> R {
        Ok(())
    }
}

impl ser::SerializeTupleVariant for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> R {
        value.serialize(FiniteCheck)
    }
    fn end(self) -> R {
        Ok(())
    }
}

impl ser::SerializeMap for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> R {
        key.serialize(FiniteCheck)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> R {
        value.serialize(FiniteCheck)
    }
    fn end(self) -> R {
        Ok(())
    }
}

impl ser::SerializeStruct for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, value: &T) -> R {
        value.serialize(FiniteCheck)
    }
    fn end(self) -> R {
        Ok(())
    }
}

impl ser::SerializeStructVariant for FiniteCheck {
    type Ok = ();
    type Error = CheckError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, value: &T) -> R {
        value.serialize(FiniteCheck)
    }
    fn end(self) -> R {
        Ok(())
    }
}

