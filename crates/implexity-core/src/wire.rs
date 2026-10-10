// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

