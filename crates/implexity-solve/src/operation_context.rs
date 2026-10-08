// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::error::{CaeError, CaeResult};
use implexity_core::json::canonical_sha256;
use serde_json::json;

use crate::trace::Fields;

pub const SCOPE_SCHEMA: &str = "implexity-operation-scope/1";

fn canonical_text(value: &str, label: &str) -> CaeResult<String> {
    if value.is_empty()
        || value != value.trim()
        || value.chars().count() > 256
        || value.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
    {
        return Err(CaeError::contract(format!(
            "{label} must be nonempty canonical text of at most 256 characters"
        )));
    }
    Ok(value.to_string())
}



pub fn checked_digest(value: &str, label: &str) -> CaeResult<String> {
    let value = canonical_text(value, label)?;
    if !is_sha256_hex(&value) {
        return Err(CaeError::contract(format!("{label} must be a lowercase SHA-256 digest")));
    }
    Ok(value)
}

#[must_use]
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OperationExecutionContext {
    operation_id: String,
    purpose: String,
    scope_digest: String,
    provider_profile_digest: String,
    authority_eligible: bool,
}

impl OperationExecutionContext {


    pub fn new(
        operation_id: &str,
        purpose: &str,
        scope_digest: &str,
        provider_profile_digest: &str,
        authority_eligible: bool,
    ) -> CaeResult<Self> {
        Ok(Self {
            operation_id: canonical_text(operation_id, "operation id")?,
            purpose: canonical_text(purpose, "operation purpose")?,
            scope_digest: checked_digest(scope_digest, "operation scope digest")?,
            provider_profile_digest: checked_digest(provider_profile_digest, "provider profile digest")?,
            authority_eligible,
        })
    }



    pub fn root(
        operation_id: &str,
        purpose: &str,
        provider_profile_digest: &str,
        authority_eligible: bool,
    ) -> CaeResult<Self> {
        let operation_id = canonical_text(operation_id, "operation id")?;
        let purpose = canonical_text(purpose, "operation purpose")?;
        let provider = checked_digest(provider_profile_digest, "provider profile digest")?;
        let scope = canonical_sha256(&json!({
            "schema": SCOPE_SCHEMA,
            "kind": "root",
            "operation_id": operation_id,
            "purpose": purpose,
            "provider_profile_digest": provider,
            "authority_eligible": authority_eligible,
        }));
        Ok(Self {
            operation_id,
            purpose,
            scope_digest: scope,
            provider_profile_digest: provider,
            authority_eligible,
        })
    }



    pub fn child(&self, purpose: &str, discriminator: &str, authority_eligible: bool) -> CaeResult<Self> {
        let purpose = canonical_text(purpose, "child operation purpose")?;
        let discriminator = canonical_text(discriminator, "child operation discriminator")?;
        if authority_eligible && !self.authority_eligible {
            return Err(CaeError::contract(
                "a non-authority operation scope cannot create an authority child",
            ));
        }
        let scope = canonical_sha256(&json!({
            "schema": SCOPE_SCHEMA,
            "kind": "child",
            "operation_id": self.operation_id,
            "parent_scope_digest": self.scope_digest,
            "purpose": purpose,
            "discriminator": discriminator,
            "provider_profile_digest": self.provider_profile_digest,
            "authority_eligible": authority_eligible,
        }));
        Ok(Self {
            operation_id: self.operation_id.clone(),
            purpose,
            scope_digest: scope,
            provider_profile_digest: self.provider_profile_digest.clone(),
            authority_eligible,
        })
    }

    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    #[must_use]
    pub fn purpose(&self) -> &str {
        &self.purpose
    }
    #[must_use]
    pub fn scope_digest(&self) -> &str {
        &self.scope_digest
    }
    #[must_use]
    pub fn provider_profile_digest(&self) -> &str {
        &self.provider_profile_digest
    }
    #[must_use]
    pub fn authority_eligible(&self) -> bool {
        self.authority_eligible
    }

    #[must_use]
    pub fn trace_fields(&self) -> Fields {
        crate::trace_fields! {
            "operation_context_operation_id" => self.operation_id,
            "operation_context_purpose" => self.purpose,
            "operation_context_scope_sha256" => self.scope_digest,
            "operation_context_provider_profile_sha256" => self.provider_profile_digest,
            "operation_context_authority_eligible" => self.authority_eligible,
        }
    }
}

#[must_use]
pub fn trace_fields_of(context: Option<&OperationExecutionContext>) -> Fields {
    context.map(OperationExecutionContext::trace_fields).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContextRequirement<'a> {
    pub authority_eligible: Option<bool>,
    pub expected: Option<&'a OperationExecutionContext>,
    pub provider_profile_digest: Option<&'a str>,
}



pub fn require_operation_context<'c>(
    value: Option<&'c OperationExecutionContext>,
    requirement: ContextRequirement<'_>,
    label: &str,
) -> CaeResult<&'c OperationExecutionContext> {
    let Some(value) = value else {
        return Err(CaeError::contract(format!("{label} must be an OperationExecutionContext")));
    };
    if let Some(required) = requirement.authority_eligible
        && value.authority_eligible != required
    {
        let state = if required { "authority-eligible" } else { "non-authority" };
        return Err(CaeError::contract(format!("{label} must be {state}")));
    }
    if let Some(expected) = requirement.expected
        && value != expected
    {
        return Err(CaeError::contract(format!("{label} does not match its bound scope")));
    }
    if let Some(profile) = requirement.provider_profile_digest {
        let profile = checked_digest(profile, "expected provider profile digest")?;
        if value.provider_profile_digest != profile {
            return Err(CaeError::contract(format!("{label} does not match the active provider profile")));
        }
    }
    Ok(value)
}

