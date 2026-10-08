// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value};

use crate::canonical::{
    evidence_sha256, f, has_exact_keys, is_canonical_text, is_digest, obj, opt_f, opt_s, s, sha256_hex,
};

const POLICY_SCHEMA: &str = "implexity-computation-effort-policy/1";
const EFFECTIVE_SCHEMA: &str = "implexity-effective-computation-effort/1";
const OBSERVED_SCHEMA: &str = "implexity-observed-computation-effort/1";
const TRUTH_SCHEMA: &str = "implexity-computation-truth-envelope/1";
const PUBLICATION_ONLY: &str = "publication_only";
pub const MAX_SAFE_JSON_INTEGER: i64 = (1 << 53) - 1;

#[must_use]
pub fn default_profile_digest() -> String {
    sha256_hex(b"implexity-default-exact-provider-profile/1")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct EffortError(pub String);

pub type EffortResult<T> = Result<T, EffortError>;

fn err<T>(message: impl Into<String>) -> EffortResult<T> {
    Err(EffortError(message.into()))
}

macro_rules! closed_enum {
    ($name:ident, $label:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }


            pub fn parse(name: &str, value: &Value) -> EffortResult<Self> {
                match value.as_str() {
                    $(Some($text) => Ok(Self::$variant),)+
                    _ => err(format!("{name} must be one of: {}", [$($text),+].join(", "))),
                }
            }
        }
    };
}

closed_enum!(ComputationMode, "mode", { Exact => "exact", VerifiedPreview => "verified_preview", InteractivePreview => "interactive_preview" });
closed_enum!(OodPolicy, "ood_policy", { Refuse => "refuse", RequireExact => "require_exact", HoldExactAnchor => "hold_exact_anchor" });
closed_enum!(TruthStatus, "truth_status", { Exact => "exact", VerifiedPreview => "verified_preview", InteractivePreview => "interactive_preview", Refused => "refused" });
closed_enum!(ExactCorrectionState, "correction_state", {
    NotApplicable => "not_applicable", Current => "current", Pending => "pending",
    Due => "due", Overdue => "overdue", Refused => "refused",
});

impl ComputationMode {
    fn fidelity(self) -> u8 {
        match self {
            Self::InteractivePreview => 0,
            Self::VerifiedPreview => 1,
            Self::Exact => 2,
        }
    }
}

fn text(name: &str, value: &str) -> EffortResult<()> {
    if value.is_empty() || value != value.trim() {
        return err(format!("{name} must be a non-empty canonical string"));
    }
    if value.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
        return err(format!("{name} must not contain control characters"));
    }
    if !is_canonical_text(value) {
        return err(format!("{name} must use canonical Unicode NFC"));
    }
    Ok(())
}

fn text_value(name: &str, value: &Value) -> EffortResult<String> {
    match value.as_str() {
        Some(v) => {
            text(name, v)?;
            Ok(v.to_string())
        }
        None => err(format!("{name} must be a non-empty canonical string")),
    }
}

fn digest(name: &str, value: &str) -> EffortResult<()> {
    text(name, value)?;
    if is_digest(value) { Ok(()) } else { err(format!("{name} must be a lowercase SHA-256 digest")) }
}

fn digest_value(name: &str, value: &Value) -> EffortResult<String> {
    let v = text_value(name, value)?;
    digest(name, &v)?;
    Ok(v)
}

fn optional_digest_value(name: &str, value: &Value) -> EffortResult<Option<String>> {
    if value.is_null() { Ok(None) } else { digest_value(name, value).map(Some) }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn optional_positive(name: &str, value: &Value) -> EffortResult<Option<f64>> {
    if value.is_null() {
        return Ok(None);
    }
    match number(value) {
        Some(v) if v.is_finite() && v > 0.0 => Ok(Some(v)),
        _ => err(format!("{name} must be positive finite or null")),
    }
}

fn nonnegative(name: &str, value: &Value) -> EffortResult<f64> {
    match number(value) {
        Some(v) if v.is_finite() && v >= 0.0 => Ok(if v == 0.0 { 0.0 } else { v }),
        _ => err(format!("{name} must be finite and non-negative")),
    }
}

fn optional_nonnegative(name: &str, value: &Value) -> EffortResult<Option<f64>> {
    if value.is_null() { Ok(None) } else { nonnegative(name, value).map(Some) }
}

fn check_nonnegative(name: &str, value: f64) -> EffortResult<f64> {
    if value.is_finite() && value >= 0.0 {
        Ok(if value == 0.0 { 0.0 } else { value })
    } else {
        err(format!("{name} must be finite and non-negative"))
    }
}

fn exact_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

fn closed<'a>(name: &str, row: &'a Value, expected: &[&str]) -> EffortResult<&'a Map<String, Value>> {
    match row.as_object() {
        Some(m) if has_exact_keys(m, expected) => Ok(m),
        _ => err(format!("{name} fields do not match the canonical schema")),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PolicyArgs<'a> {
    pub mode: &'a Value,
    pub wall_time_budget_s: &'a Value,
    pub wall_time_mode: &'a str,
    pub memory_budget_bytes: &'a Value,
    pub target_update_rate_hz: &'a Value,
    pub max_response_error: &'a Value,
    pub max_state_error: &'a Value,
    pub max_gradient_error: &'a Value,
    pub trust_radius: &'a Value,
    pub exact_correction_cadence: &'a Value,
    pub exact_correction_deadline_s: &'a Value,
    pub ood_policy: &'a Value,
    pub provider_registry_generation: i64,
    pub provider_registry_fingerprint: &'a str,
    pub provider_profile_id: &'a str,
    pub normalized_profile_digest: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputationEffortPolicy {
    pub mode: ComputationMode,
    pub wall_time_budget_s: Option<f64>,
    pub memory_budget_bytes: Option<i64>,
    pub target_update_rate_hz: Option<f64>,
    pub max_response_error: f64,
    pub max_state_error: f64,
    pub max_gradient_error: f64,
    pub trust_radius: f64,
    pub exact_correction_cadence: i64,
    pub exact_correction_deadline_s: f64,
    pub ood_policy: OodPolicy,
    pub provider_registry_generation: i64,
    pub provider_registry_fingerprint: String,
    pub provider_profile_id: String,
    pub normalized_profile_digest: String,
    pub wall_time_mode: String,
}

impl Default for ComputationEffortPolicy {
    fn default() -> Self {
        Self {
            mode: ComputationMode::Exact,
            wall_time_budget_s: None,
            memory_budget_bytes: None,
            target_update_rate_hz: None,
            max_response_error: 0.0,
            max_state_error: 0.0,
            max_gradient_error: 0.0,
            trust_radius: 0.0,
            exact_correction_cadence: 1,
            exact_correction_deadline_s: 0.0,
            ood_policy: OodPolicy::Refuse,
            provider_registry_generation: 0,
            provider_registry_fingerprint: "builtin".into(),
            provider_profile_id: "default".into(),
            normalized_profile_digest: default_profile_digest(),
            wall_time_mode: "default".into(),
        }
    }
}

impl ComputationEffortPolicy {

    pub fn checked(mut self) -> EffortResult<Self> {
        if let Some(w) = self.wall_time_budget_s
            && !(w.is_finite() && w > 0.0)
        {
            return err("wall_time_budget_s must be positive finite or null");
        }
        if self.wall_time_mode != "default" && self.wall_time_mode != "unlimited" {
            return err("wall_time_mode must be default or unlimited");
        }
        if self.wall_time_mode == "unlimited"
            && (self.wall_time_budget_s.is_some() || self.memory_budget_bytes.is_none())
        {
            return err(
                "unlimited wall time requires null wall_time_budget_s and an explicit positive memory budget",
            );
        }
        if let Some(m) = self.memory_budget_bytes {
            if m <= 0 {
                return err("memory_budget_bytes must be a positive integer or null");
            }
            if m > MAX_SAFE_JSON_INTEGER {
                return err("memory_budget_bytes exceeds the interoperable JSON integer range");
            }
        }
        if let Some(r) = self.target_update_rate_hz
            && !(r.is_finite() && r > 0.0)
        {
            return err("target_update_rate_hz must be positive finite or null");
        }
        self.max_response_error = check_nonnegative("max_response_error", self.max_response_error)?;
        self.max_state_error = check_nonnegative("max_state_error", self.max_state_error)?;
        self.max_gradient_error = check_nonnegative("max_gradient_error", self.max_gradient_error)?;
        self.trust_radius = check_nonnegative("trust_radius", self.trust_radius)?;
        if self.exact_correction_cadence < 1 || self.exact_correction_cadence > MAX_SAFE_JSON_INTEGER {
            return err("exact_correction_cadence must be a positive integer");
        }
        self.exact_correction_deadline_s =
            check_nonnegative("exact_correction_deadline_s", self.exact_correction_deadline_s)?;
        if self.provider_registry_generation < 0 {
            return err("provider_registry_generation must be a non-negative integer");
        }
        if self.provider_registry_generation > MAX_SAFE_JSON_INTEGER {
            return err("provider_registry_generation exceeds the interoperable JSON integer range");
        }
        text("provider_registry_fingerprint", &self.provider_registry_fingerprint)?;
        text("provider_profile_id", &self.provider_profile_id)?;
        digest("normalized_profile_digest", &self.normalized_profile_digest)?;
        if self.mode == ComputationMode::Exact {
            if [
                self.max_response_error,
                self.max_state_error,
                self.max_gradient_error,
                self.trust_radius,
                self.exact_correction_deadline_s,
            ]
            .iter()
            .any(|v| *v != 0.0)
            {
                return err("exact mode requires zero error bounds, trust radius, and correction deadline");
            }
            if self.exact_correction_cadence != 1 {
                return err("exact mode requires correction cadence one");
            }
        } else {
            if self.trust_radius <= 0.0 {
                return err("preview modes require a positive trust_radius");
            }
            if self.exact_correction_deadline_s <= 0.0 {
                return err("preview modes require a positive exact_correction_deadline_s");
            }
        }
        Ok(self)
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut budgets = Map::new();
        budgets.insert("wall_time_s".into(), opt_f(self.wall_time_budget_s));
        budgets.insert("memory_bytes".into(), self.memory_budget_bytes.map_or(Value::Null, Value::from));
        budgets.insert("target_update_rate_hz".into(), opt_f(self.target_update_rate_hz));
        budgets.insert("target_update_rate_role".into(), s(PUBLICATION_ONLY));
        if self.wall_time_mode == "unlimited" {
            budgets.insert("wall_time_mode".into(), s("unlimited"));
        }
        obj([
            ("schema", s(POLICY_SCHEMA)),
            ("mode", s(self.mode.as_str())),
            ("budgets", Value::Object(budgets)),
            (
                "error_limits",
                obj([
                    ("response", f(self.max_response_error)),
                    ("state", f(self.max_state_error)),
                    ("gradient", f(self.max_gradient_error)),
                ]),
            ),
            ("trust_radius", f(self.trust_radius)),
            (
                "exact_correction",
                obj([
                    ("cadence_updates", Value::from(self.exact_correction_cadence)),
                    ("deadline_s", f(self.exact_correction_deadline_s)),
                ]),
            ),
            ("ood_policy", s(self.ood_policy.as_str())),
            (
                "provider_profile",
                obj([
                    ("registry_generation", Value::from(self.provider_registry_generation)),
                    ("registry_fingerprint", s(&self.provider_registry_fingerprint)),
                    ("profile_id", s(&self.provider_profile_id)),
                    ("normalized_digest", s(&self.normalized_profile_digest)),
                ]),
            ),
        ])
    }


    pub fn from_wire(row: &Value) -> EffortResult<Self> {
        let row = closed(
            "computation effort",
            row,
            &[
                "schema",
                "mode",
                "budgets",
                "error_limits",
                "trust_radius",
                "exact_correction",
                "ood_policy",
                "provider_profile",
            ],
        )?;
        if row["schema"].as_str() != Some(POLICY_SCHEMA) {
            return err("unsupported computation effort schema");
        }
        let mut budget_row = row["budgets"].clone();
        let mut wall_time_mode = "default".to_string();
        if let Value::Object(b) = &mut budget_row
            && let Some(mode) = b.shift_remove("wall_time_mode")
        {
            match mode.as_str() {
                Some(m @ ("default" | "unlimited")) => wall_time_mode = m.to_string(),
                _ => return err("wall_time_mode must be default or unlimited"),
            }
        }
        let budgets = closed(
            "computation effort budgets",
            &budget_row,
            &["wall_time_s", "memory_bytes", "target_update_rate_hz", "target_update_rate_role"],
        )?;
        if budgets["target_update_rate_role"].as_str() != Some(PUBLICATION_ONLY) {
            return err("target update rate must remain publication-only");
        }
        let limits = closed(
            "computation effort error limits",
            &row["error_limits"],
            &["response", "state", "gradient"],
        )?;
        let correction = closed(
            "computation effort exact correction",
            &row["exact_correction"],
            &["cadence_updates", "deadline_s"],
        )?;
        let profile = closed(
            "computation effort provider profile",
            &row["provider_profile"],
            &["registry_generation", "registry_fingerprint", "profile_id", "normalized_digest"],
        )?;
        let mode = ComputationMode::parse("mode", &row["mode"])?;
        let ood = OodPolicy::parse("ood_policy", &row["ood_policy"])?;
        let memory = match &budgets["memory_bytes"] {
            Value::Null => None,
            v => Some(exact_int(v).filter(|m| *m > 0).ok_or_else(|| {
                EffortError("memory_budget_bytes must be a positive integer or null".into())
            })?),
        };
        let cadence = exact_int(&correction["cadence_updates"])
            .ok_or_else(|| EffortError("exact_correction_cadence must be a positive integer".into()))?;
        let generation = exact_int(&profile["registry_generation"]).ok_or_else(|| {
            EffortError("provider_registry_generation must be a non-negative integer".into())
        })?;
        Self {
            mode,
            wall_time_budget_s: optional_positive("wall_time_budget_s", &budgets["wall_time_s"])?,
            memory_budget_bytes: memory,
            target_update_rate_hz: optional_positive(
                "target_update_rate_hz",
                &budgets["target_update_rate_hz"],
            )?,
            max_response_error: nonnegative("max_response_error", &limits["response"])?,
            max_state_error: nonnegative("max_state_error", &limits["state"])?,
            max_gradient_error: nonnegative("max_gradient_error", &limits["gradient"])?,
            trust_radius: nonnegative("trust_radius", &row["trust_radius"])?,
            exact_correction_cadence: cadence,
            exact_correction_deadline_s: nonnegative(
                "exact_correction_deadline_s",
                &correction["deadline_s"],
            )?,
            ood_policy: ood,
            provider_registry_generation: generation,
            provider_registry_fingerprint: text_value(
                "provider_registry_fingerprint",
                &profile["registry_fingerprint"],
            )?,
            provider_profile_id: text_value("provider_profile_id", &profile["profile_id"])?,
            normalized_profile_digest: digest_value(
                "normalized_profile_digest",
                &profile["normalized_digest"],
            )?,
            wall_time_mode,
        }
        .checked()
    }


    pub fn from_args(args: &PolicyArgs<'_>) -> EffortResult<Self> {
        let memory = match args.memory_budget_bytes {
            Value::Null => None,
            v => Some(exact_int(v).filter(|m| *m > 0).ok_or_else(|| {
                EffortError("memory_budget_bytes must be a positive integer or null".into())
            })?),
        };
        let cadence = exact_int(args.exact_correction_cadence)
            .ok_or_else(|| EffortError("exact_correction_cadence must be a positive integer".into()))?;
        Self {
            mode: ComputationMode::parse("mode", args.mode)?,
            wall_time_budget_s: optional_positive("wall_time_budget_s", args.wall_time_budget_s)?,
            memory_budget_bytes: memory,
            target_update_rate_hz: optional_positive("target_update_rate_hz", args.target_update_rate_hz)?,
            max_response_error: nonnegative("max_response_error", args.max_response_error)?,
            max_state_error: nonnegative("max_state_error", args.max_state_error)?,
            max_gradient_error: nonnegative("max_gradient_error", args.max_gradient_error)?,
            trust_radius: nonnegative("trust_radius", args.trust_radius)?,
            exact_correction_cadence: cadence,
            exact_correction_deadline_s: nonnegative(
                "exact_correction_deadline_s",
                args.exact_correction_deadline_s,
            )?,
            ood_policy: OodPolicy::parse("ood_policy", args.ood_policy)?,
            provider_registry_generation: args.provider_registry_generation,
            provider_registry_fingerprint: args.provider_registry_fingerprint.to_string(),
            provider_profile_id: args.provider_profile_id.to_string(),
            normalized_profile_digest: args.normalized_profile_digest.to_string(),
            wall_time_mode: args.wall_time_mode.to_string(),
        }
        .checked()
    }


    pub fn with_identity(
        &self,
        generation: i64,
        fingerprint: &str,
        profile_id: &str,
        digest: &str,
    ) -> EffortResult<Self> {
        Self {
            provider_registry_generation: generation,
            provider_registry_fingerprint: fingerprint.to_string(),
            provider_profile_id: profile_id.to_string(),
            normalized_profile_digest: digest.to_string(),
            ..self.clone()
        }
        .checked()
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        evidence_sha256(&self.to_wire())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveComputationEffort {
    pub requested: ComputationEffortPolicy,
    pub effective: ComputationEffortPolicy,
    pub selection_reason: String,
}

impl EffectiveComputationEffort {

    pub fn new(
        requested: ComputationEffortPolicy,
        effective: ComputationEffortPolicy,
        selection_reason: &str,
    ) -> EffortResult<Self> {
        text("selection_reason", selection_reason)?;
        if effective.mode.fidelity() < requested.mode.fidelity() {
            return err("effective policy must never downgrade requested fidelity");
        }
        let profile = |p: &ComputationEffortPolicy| {
            (
                p.provider_registry_generation,
                p.provider_registry_fingerprint.clone(),
                p.provider_profile_id.clone(),
                p.normalized_profile_digest.clone(),
            )
        };
        if profile(&effective) != profile(&requested) {
            return err("effective provider profile must match the requested registry-bound profile");
        }
        if effective.ood_policy != requested.ood_policy {
            return err("effective ood_policy must match the requested refusal policy");
        }
        #[allow(clippy::float_cmp)]
        let rate_changed = effective.target_update_rate_hz != requested.target_update_rate_hz;
        if rate_changed {
            return err("effective target_update_rate_hz must preserve the publication target");
        }
        if let Some(r) = requested.wall_time_budget_s
            && effective.wall_time_budget_s.is_none_or(|e| e > r)
        {
            return err("effective wall_time_budget_s must not relax the requested budget");
        }
        if let Some(r) = requested.memory_budget_bytes
            && effective.memory_budget_bytes.is_none_or(|e| e > r)
        {
            return err("effective memory_budget_bytes must not relax the requested budget");
        }
        if effective.wall_time_mode != requested.wall_time_mode {
            return err("effective wall_time_mode must preserve the explicit requested supervision mode");
        }
        for (name, e, r) in [
            ("max_response_error", effective.max_response_error, requested.max_response_error),
            ("max_state_error", effective.max_state_error, requested.max_state_error),
            ("max_gradient_error", effective.max_gradient_error, requested.max_gradient_error),
            ("trust_radius", effective.trust_radius, requested.trust_radius),
            (
                "exact_correction_deadline_s",
                effective.exact_correction_deadline_s,
                requested.exact_correction_deadline_s,
            ),
        ] {
            if e > r {
                return err(format!("effective {name} must not relax the requested limit"));
            }
        }
        if effective.exact_correction_cadence > requested.exact_correction_cadence {
            return err("effective exact_correction_cadence must not be less frequent");
        }
        Ok(Self { requested, effective, selection_reason: selection_reason.to_string() })
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        obj([
            ("schema", s(EFFECTIVE_SCHEMA)),
            ("requested", self.requested.to_wire()),
            ("effective", self.effective.to_wire()),
            ("selection_reason", s(&self.selection_reason)),
        ])
    }


    pub fn from_wire(row: &Value) -> EffortResult<Self> {
        let row = closed(
            "effective computation effort",
            row,
            &["schema", "requested", "effective", "selection_reason"],
        )?;
        if row["schema"].as_str() != Some(EFFECTIVE_SCHEMA) {
            return err("unsupported effective computation effort schema");
        }
        let reason = text_value("selection_reason", &row["selection_reason"])?;
        Self::new(
            ComputationEffortPolicy::from_wire(&row["requested"])?,
            ComputationEffortPolicy::from_wire(&row["effective"])?,
            &reason,
        )
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        evidence_sha256(&self.to_wire())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservedComputationEffort {
    pub requested_policy_digest: String,
    pub effective_effort_digest: String,
    pub normalized_profile_digest: String,
    pub wall_time_s: Option<f64>,
    pub peak_memory_bytes: Option<i64>,
    pub published_update_rate_hz: Option<f64>,
    pub updates_since_exact: i64,
    pub exact_correction_age_s: Option<f64>,
}

impl ObservedComputationEffort {

    pub fn checked(mut self) -> EffortResult<Self> {
        digest("requested_policy_digest", &self.requested_policy_digest)?;
        digest("effective_effort_digest", &self.effective_effort_digest)?;
        digest("normalized_profile_digest", &self.normalized_profile_digest)?;
        self.wall_time_s = self.wall_time_s.map(|w| check_nonnegative("wall_time_s", w)).transpose()?;
        if let Some(m) = self.peak_memory_bytes
            && !(0..=MAX_SAFE_JSON_INTEGER).contains(&m)
        {
            return err("peak_memory_bytes must be a non-negative integer or null");
        }
        self.published_update_rate_hz = self
            .published_update_rate_hz
            .map(|r| check_nonnegative("published_update_rate_hz", r))
            .transpose()?;
        if !(0..=MAX_SAFE_JSON_INTEGER).contains(&self.updates_since_exact) {
            return err("updates_since_exact must be a non-negative integer");
        }
        self.exact_correction_age_s = self
            .exact_correction_age_s
            .map(|a| check_nonnegative("exact_correction_age_s", a))
            .transpose()?;
        Ok(self)
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        obj([
            ("schema", s(OBSERVED_SCHEMA)),
            (
                "bindings",
                obj([
                    ("requested_policy_digest", s(&self.requested_policy_digest)),
                    ("effective_effort_digest", s(&self.effective_effort_digest)),
                    ("normalized_profile_digest", s(&self.normalized_profile_digest)),
                ]),
            ),
            (
                "observed",
                obj([
                    ("wall_time_s", opt_f(self.wall_time_s)),
                    ("peak_memory_bytes", self.peak_memory_bytes.map_or(Value::Null, Value::from)),
                    ("published_update_rate_hz", opt_f(self.published_update_rate_hz)),
                    ("published_update_rate_role", s(PUBLICATION_ONLY)),
                    ("updates_since_exact", Value::from(self.updates_since_exact)),
                    ("exact_correction_age_s", opt_f(self.exact_correction_age_s)),
                ]),
            ),
        ])
    }


    pub fn from_wire(row: &Value) -> EffortResult<Self> {
        let row = closed("observed computation effort", row, &["schema", "bindings", "observed"])?;
        if row["schema"].as_str() != Some(OBSERVED_SCHEMA) {
            return err("unsupported observed computation effort schema");
        }
        let b = closed(
            "observed computation bindings",
            &row["bindings"],
            &["requested_policy_digest", "effective_effort_digest", "normalized_profile_digest"],
        )?;
        let o = closed(
            "observed computation values",
            &row["observed"],
            &[
                "wall_time_s",
                "peak_memory_bytes",
                "published_update_rate_hz",
                "published_update_rate_role",
                "updates_since_exact",
                "exact_correction_age_s",
            ],
        )?;
        if o["published_update_rate_role"].as_str() != Some(PUBLICATION_ONLY) {
            return err("observed update rate must remain publication-only");
        }
        let peak = match &o["peak_memory_bytes"] {
            Value::Null => None,
            v => Some(exact_int(v).ok_or_else(|| {
                EffortError("peak_memory_bytes must be a non-negative integer or null".into())
            })?),
        };
        Self {
            requested_policy_digest: digest_value("requested_policy_digest", &b["requested_policy_digest"])?,
            effective_effort_digest: digest_value("effective_effort_digest", &b["effective_effort_digest"])?,
            normalized_profile_digest: digest_value(
                "normalized_profile_digest",
                &b["normalized_profile_digest"],
            )?,
            wall_time_s: optional_nonnegative("wall_time_s", &o["wall_time_s"])?,
            peak_memory_bytes: peak,
            published_update_rate_hz: optional_nonnegative(
                "published_update_rate_hz",
                &o["published_update_rate_hz"],
            )?,
            updates_since_exact: exact_int(&o["updates_since_exact"])
                .ok_or_else(|| EffortError("updates_since_exact must be a non-negative integer".into()))?,
            exact_correction_age_s: optional_nonnegative(
                "exact_correction_age_s",
                &o["exact_correction_age_s"],
            )?,
        }
        .checked()
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        evidence_sha256(&self.to_wire())
    }


    pub fn validate_against(&self, effective: &EffectiveComputationEffort) -> EffortResult<()> {
        if self.requested_policy_digest != effective.requested.sha256() {
            return err("observed requested-policy mismatch");
        }
        if self.effective_effort_digest != effective.sha256() {
            return err("observed effective-effort mismatch");
        }
        if self.normalized_profile_digest != effective.effective.normalized_profile_digest {
            return err("observed provider-profile mismatch");
        }
        Ok(())
    }


    pub fn hard_budget_violations(
        &self,
        effective: &EffectiveComputationEffort,
    ) -> EffortResult<Vec<&'static str>> {
        self.validate_against(effective)?;
        let policy = &effective.effective;
        let mut violations = Vec::new();
        match self.wall_time_s {
            None => violations.push("wall_time_unverifiable"),
            Some(w) if policy.wall_time_budget_s.is_some_and(|b| w > b) => {
                violations.push("wall_time_exceeded");
            }
            Some(_) => {}
        }
        match self.peak_memory_bytes {
            None => violations.push("peak_memory_unverifiable"),
            Some(m) if policy.memory_budget_bytes.is_some_and(|b| m > b) => {
                violations.push("peak_memory_exceeded");
            }
            Some(_) => {}
        }
        Ok(violations)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TruthEnvelope {
    pub mode: ComputationMode,
    pub truth_status: TruthStatus,
    pub requested_policy_digest: String,
    pub effective_effort_digest: String,
    pub observed_effort_digest: String,
    pub provider_registry_generation: i64,
    pub provider_registry_fingerprint: String,
    pub provider_profile_id: String,
    pub normalized_profile_digest: String,
    pub result_digest: Option<String>,
    pub exact_anchor_digest: Option<String>,
    pub exact_correction_digest: Option<String>,
    pub correction_state: ExactCorrectionState,
    pub calibration_digest: Option<String>,
    pub confidence: f64,
    pub response_error_bound: f64,
    pub state_error_bound: f64,
    pub gradient_error_bound: f64,
    pub trust_distance: f64,
    pub out_of_distribution: bool,
    pub refusal_reason: Option<String>,
}

impl TruthEnvelope {

    #[allow(clippy::too_many_lines)]
    pub fn checked(mut self) -> EffortResult<Self> {
        for (name, v) in [
            ("requested_policy_digest", &self.requested_policy_digest),
            ("effective_effort_digest", &self.effective_effort_digest),
            ("observed_effort_digest", &self.observed_effort_digest),
            ("normalized_profile_digest", &self.normalized_profile_digest),
        ] {
            digest(name, v)?;
        }
        if self.provider_registry_generation < 0 {
            return err("provider_registry_generation must be a non-negative integer");
        }
        if self.provider_registry_generation > MAX_SAFE_JSON_INTEGER {
            return err("provider_registry_generation exceeds the interoperable JSON integer range");
        }
        text("provider_registry_fingerprint", &self.provider_registry_fingerprint)?;
        text("provider_profile_id", &self.provider_profile_id)?;
        for (name, v) in [
            ("result_digest", &self.result_digest),
            ("exact_anchor_digest", &self.exact_anchor_digest),
            ("exact_correction_digest", &self.exact_correction_digest),
            ("calibration_digest", &self.calibration_digest),
        ] {
            if let Some(v) = v {
                digest(name, v)?;
            }
        }
        self.confidence = check_nonnegative("confidence", self.confidence)?;
        if self.confidence > 1.0 {
            return err("confidence must not exceed one");
        }
        self.response_error_bound = check_nonnegative("response_error_bound", self.response_error_bound)?;
        self.state_error_bound = check_nonnegative("state_error_bound", self.state_error_bound)?;
        self.gradient_error_bound = check_nonnegative("gradient_error_bound", self.gradient_error_bound)?;
        self.trust_distance = check_nonnegative("trust_distance", self.trust_distance)?;
        if let Some(r) = &self.refusal_reason {
            text("refusal_reason", r)?;
        }
        let refused = self.truth_status == TruthStatus::Refused;
        let uncertainty = [
            self.response_error_bound,
            self.state_error_bound,
            self.gradient_error_bound,
            self.trust_distance,
        ];
        if refused {
            if self.result_digest.is_some() {
                return err("refused truth must not carry a result digest");
            }
            if self.refusal_reason.as_deref().is_none_or(str::is_empty) {
                return err("refused truth requires a refusal_reason");
            }
            if self.confidence != 0.0 || uncertainty.iter().any(|v| *v != 0.0) {
                return err("refused truth requires zero confidence, uncertainty, and trust distance");
            }
            if self.exact_correction_digest.is_some() {
                return err("refused truth must not claim an exact-correction result");
            }
        } else {
            if self.result_digest.is_none() {
                return err("non-refused truth requires a result digest");
            }
            if self.refusal_reason.is_some() || self.out_of_distribution {
                return err("a returned result cannot also be marked refused or out of distribution");
            }
        }
        if self.mode == ComputationMode::Exact {
            if !refused && self.truth_status != TruthStatus::Exact {
                return err("exact mode requires exact or refused truth status");
            }
            if uncertainty.iter().any(|v| *v != 0.0) {
                return err("exact truth requires zero uncertainty and trust distance");
            }
            if self.exact_anchor_digest.is_some()
                || self.exact_correction_digest.is_some()
                || self.calibration_digest.is_some()
            {
                return err("exact truth must not depend on preview anchor, correction, or calibration");
            }
            if self.out_of_distribution {
                return err("exact truth cannot be classified as preview out-of-distribution");
            }
            if self.correction_state != ExactCorrectionState::NotApplicable {
                return err("exact truth requires not-applicable correction state");
            }
            #[allow(clippy::float_cmp)]
            if !refused && self.confidence != 1.0 {
                return err("exact truth requires confidence one");
            }
        } else {
            if self.exact_anchor_digest.is_none() {
                return err("preview truth requires an exact anchor digest");
            }
            if refused {
                if !matches!(
                    self.correction_state,
                    ExactCorrectionState::Due | ExactCorrectionState::Overdue | ExactCorrectionState::Refused
                ) {
                    return err("refused preview requires due, overdue, or refused correction state");
                }
            } else {
                let expected = if self.mode == ComputationMode::VerifiedPreview {
                    TruthStatus::VerifiedPreview
                } else {
                    TruthStatus::InteractivePreview
                };
                if self.truth_status != expected {
                    return err("preview mode and truth status must match");
                }
                if self.calibration_digest.is_none() || self.confidence <= 0.0 {
                    return err("preview truth requires calibrated errors and positive confidence");
                }
                if !matches!(
                    self.correction_state,
                    ExactCorrectionState::Current | ExactCorrectionState::Pending
                ) {
                    return err("returned preview requires current or pending correction state");
                }
                if self.correction_state == ExactCorrectionState::Current
                    && self.exact_correction_digest.is_none()
                {
                    return err("current preview requires an exact-correction digest");
                }
                if self.mode == ComputationMode::VerifiedPreview
                    && (self.correction_state != ExactCorrectionState::Current
                        || self.exact_correction_digest.is_none())
                {
                    return err("verified preview requires a current exact-correction digest");
                }
            }
        }
        Ok(self)
    }

    #[must_use]
    pub fn exact_truth(&self) -> bool {
        self.mode == ComputationMode::Exact && self.truth_status == TruthStatus::Exact
    }


    pub fn validate_against(
        &self,
        effective: &EffectiveComputationEffort,
        observed: &ObservedComputationEffort,
    ) -> EffortResult<()> {
        observed.validate_against(effective)?;
        let policy = &effective.effective;
        let checks: [(bool, &str); 8] = [
            (self.requested_policy_digest == effective.requested.sha256(), "requested policy"),
            (self.effective_effort_digest == effective.sha256(), "effective effort"),
            (self.observed_effort_digest == observed.sha256(), "observed effort"),
            (self.normalized_profile_digest == policy.normalized_profile_digest, "profile digest"),
            (self.provider_registry_generation == policy.provider_registry_generation, "registry generation"),
            (
                self.provider_registry_fingerprint == policy.provider_registry_fingerprint,
                "registry fingerprint",
            ),
            (self.provider_profile_id == policy.provider_profile_id, "provider profile"),
            (self.mode == policy.mode, "computation mode"),
        ];
        for (ok, label) in checks {
            if !ok {
                return err(format!("truth {label} mismatch"));
            }
        }
        if self.truth_status == TruthStatus::Refused {
            return Ok(());
        }
        let violations = observed.hard_budget_violations(effective)?;
        if !violations.is_empty() {
            return err(format!(
                "non-refused truth violates hard computation budget: {}",
                violations.join(", ")
            ));
        }
        for (label, actual, maximum) in [
            ("response", self.response_error_bound, policy.max_response_error),
            ("state", self.state_error_bound, policy.max_state_error),
            ("gradient", self.gradient_error_bound, policy.max_gradient_error),
            ("trust", self.trust_distance, policy.trust_radius),
        ] {
            if actual > maximum {
                return err(format!("truth {label} bound exceeds the effective policy"));
            }
        }
        if self.mode != ComputationMode::Exact {
            if observed.updates_since_exact >= policy.exact_correction_cadence {
                return err("returned preview exceeded exact-correction cadence");
            }
            if observed.exact_correction_age_s.is_none_or(|a| a > policy.exact_correction_deadline_s) {
                return err("returned preview exceeded exact-correction deadline");
            }
        }
        Ok(())
    }

    fn classification(&self) -> Value {
        let authority = if self.exact_truth() {
            "exact_evidence_only"
        } else if self.truth_status == TruthStatus::Refused {
            "refused"
        } else {
            "preview_only"
        };
        obj([
            ("mode", s(self.mode.as_str())),
            ("truth_status", s(self.truth_status.as_str())),
            ("authority", s(authority)),
            ("allowed_authoritative_actions_without_admission", Value::Array(Vec::new())),
        ])
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        obj([
            ("schema", s(TRUTH_SCHEMA)),
            ("classification", self.classification()),
            (
                "bindings",
                obj([
                    ("requested_policy_digest", s(&self.requested_policy_digest)),
                    ("effective_effort_digest", s(&self.effective_effort_digest)),
                    ("observed_effort_digest", s(&self.observed_effort_digest)),
                    ("provider_registry_generation", Value::from(self.provider_registry_generation)),
                    ("provider_registry_fingerprint", s(&self.provider_registry_fingerprint)),
                    ("provider_profile_id", s(&self.provider_profile_id)),
                    ("normalized_profile_digest", s(&self.normalized_profile_digest)),
                    ("result_digest", opt_s(self.result_digest.as_deref())),
                    ("exact_anchor_digest", opt_s(self.exact_anchor_digest.as_deref())),
                    ("exact_correction_digest", opt_s(self.exact_correction_digest.as_deref())),
                    ("calibration_digest", opt_s(self.calibration_digest.as_deref())),
                ]),
            ),
            (
                "uncertainty",
                obj([
                    ("confidence", f(self.confidence)),
                    ("response_error_bound", f(self.response_error_bound)),
                    ("state_error_bound", f(self.state_error_bound)),
                    ("gradient_error_bound", f(self.gradient_error_bound)),
                    ("trust_distance", f(self.trust_distance)),
                ]),
            ),
            ("correction", obj([("state", s(self.correction_state.as_str()))])),
            (
                "refusal",
                obj([
                    ("out_of_distribution", Value::Bool(self.out_of_distribution)),
                    ("reason", opt_s(self.refusal_reason.as_deref())),
                ]),
            ),
        ])
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        evidence_sha256(&self.to_wire())
    }


    pub fn from_wire(row: &Value) -> EffortResult<Self> {
        let row = closed(
            "computation truth",
            row,
            &["schema", "classification", "bindings", "uncertainty", "correction", "refusal"],
        )?;
        if row["schema"].as_str() != Some(TRUTH_SCHEMA) {
            return err("unsupported computation truth schema");
        }
        let classification = closed(
            "truth classification",
            &row["classification"],
            &["mode", "truth_status", "authority", "allowed_authoritative_actions_without_admission"],
        )?;
        let b = closed(
            "truth bindings",
            &row["bindings"],
            &[
                "requested_policy_digest",
                "effective_effort_digest",
                "observed_effort_digest",
                "provider_registry_generation",
                "provider_registry_fingerprint",
                "provider_profile_id",
                "normalized_profile_digest",
                "result_digest",
                "exact_anchor_digest",
                "exact_correction_digest",
                "calibration_digest",
            ],
        )?;
        let u = closed(
            "truth uncertainty",
            &row["uncertainty"],
            &[
                "confidence",
                "response_error_bound",
                "state_error_bound",
                "gradient_error_bound",
                "trust_distance",
            ],
        )?;
        let c = closed("truth correction", &row["correction"], &["state"])?;
        let r = closed("truth refusal", &row["refusal"], &["out_of_distribution", "reason"])?;
        let refusal_reason = match &r["reason"] {
            Value::Null => None,
            v => Some(text_value("refusal_reason", v)?),
        };
        let Value::Bool(ood) = r["out_of_distribution"] else {
            return err("out_of_distribution must be boolean");
        };
        let generation = exact_int(&b["provider_registry_generation"]).ok_or_else(|| {
            EffortError("provider_registry_generation must be a non-negative integer".into())
        })?;
        let envelope = Self {
            mode: ComputationMode::parse("mode", &classification["mode"])?,
            truth_status: TruthStatus::parse("truth_status", &classification["truth_status"])?,
            requested_policy_digest: digest_value("requested_policy_digest", &b["requested_policy_digest"])?,
            effective_effort_digest: digest_value("effective_effort_digest", &b["effective_effort_digest"])?,
            observed_effort_digest: digest_value("observed_effort_digest", &b["observed_effort_digest"])?,
            provider_registry_generation: generation,
            provider_registry_fingerprint: text_value(
                "provider_registry_fingerprint",
                &b["provider_registry_fingerprint"],
            )?,
            provider_profile_id: text_value("provider_profile_id", &b["provider_profile_id"])?,
            normalized_profile_digest: digest_value(
                "normalized_profile_digest",
                &b["normalized_profile_digest"],
            )?,
            result_digest: optional_digest_value("result_digest", &b["result_digest"])?,
            exact_anchor_digest: optional_digest_value("exact_anchor_digest", &b["exact_anchor_digest"])?,
            exact_correction_digest: optional_digest_value(
                "exact_correction_digest",
                &b["exact_correction_digest"],
            )?,
            correction_state: ExactCorrectionState::parse("correction_state", &c["state"])?,
            calibration_digest: optional_digest_value("calibration_digest", &b["calibration_digest"])?,
            confidence: nonnegative("confidence", &u["confidence"])?,
            response_error_bound: nonnegative("response_error_bound", &u["response_error_bound"])?,
            state_error_bound: nonnegative("state_error_bound", &u["state_error_bound"])?,
            gradient_error_bound: nonnegative("gradient_error_bound", &u["gradient_error_bound"])?,
            trust_distance: nonnegative("trust_distance", &u["trust_distance"])?,
            out_of_distribution: ood,
            refusal_reason,
        }
        .checked()?;
        if !implexity_core::pyobj::py_eq(&Value::Object(classification.clone()), &envelope.classification()) {
            return err("wire authority classification disagrees with derived truth");
        }
        Ok(envelope)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultWithTruth {
    payload_bytes: Vec<u8>,
    truth: TruthEnvelope,
}

impl ResultWithTruth {

    pub fn new(payload_bytes: Vec<u8>, truth: TruthEnvelope) -> EffortResult<Self> {
        if truth.truth_status == TruthStatus::Refused {
            return err("refused computation cannot carry a result payload");
        }
        if Some(sha256_hex(&payload_bytes)) != truth.result_digest {
            return err("canonical payload digest does not match truth evidence");
        }
        Ok(Self { payload_bytes, truth })
    }

    #[must_use]
    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload_bytes
    }

    #[must_use]
    pub fn truth(&self) -> &TruthEnvelope {
        &self.truth
    }

    #[must_use]
    pub fn payload_digest(&self) -> String {
        sha256_hex(&self.payload_bytes)
    }
}

