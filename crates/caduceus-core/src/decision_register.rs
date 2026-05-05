//! Decision register types — first-class structured state for plan-mode
//! locked decisions, per `spec-decision-register` (P-tier).
//!
//! This module provides the data types only; the reducer + persistence +
//! restore protocol live in `caduceus-orchestrator`. Wire shapes are stable
//! and round-trip via serde under the same conventions as `AgentEvent`.
//!
//! See `docs/specs/spec-decision-register.md` §3.1, §3.2, §4.1, §4.2 for
//! the normative contract.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use crate::{ExecutionId, RequestId, SessionId};

// ── Identifiers ──────────────────────────────────────────────────────────────

/// Stable, agent-scoped string identifier for one locked decision. ASCII,
/// lower-kebab path-segments, ≤128 bytes, matches `^[a-z0-9][a-z0-9_/-]{0,127}$`.
/// See spec §3.1.2 / Z8-D2..D3.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DecisionId(pub String);

impl DecisionId {
    /// Construct after validating against Z8-D3. Returns `Err` with a stable
    /// reason string on violation; callers MUST surface this as a
    /// `DecisionRegisterError { code: "invalid-id" }`.
    pub fn new(s: impl Into<String>) -> Result<Self, &'static str> {
        let s = s.into();
        if s.is_empty() {
            return Err("DecisionId: empty");
        }
        if s.len() > 128 {
            return Err("DecisionId: exceeds 128 bytes");
        }
        let bytes = s.as_bytes();
        // First char: [a-z0-9]
        let first = bytes[0];
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err("DecisionId: first byte must be [a-z0-9]");
        }
        // Remaining: [a-z0-9_/-]
        for &b in &bytes[1..] {
            let ok =
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'/' || b == b'-';
            if !ok {
                return Err("DecisionId: only [a-z0-9_/-] allowed");
            }
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `ThreadId` — durable storage key, decoupled from `SessionId`.
/// Survives session rebinds and workspace mutations. See spec §3.0 / Z8-D40..D43.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(pub Uuid);

impl ThreadId {
    /// Mint a fresh `ThreadId` (UUID v4 — caduceus does not require time-
    /// ordered UUIDs here; ordering is event-driven).
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Construct from an existing UUID (for tests, on-disk reads, migration).
    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }
}

impl Default for ThreadId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identifies the process / surface that emitted an event. Used for
/// cross-process ordering when multiple agent runners are attached to the
/// same thread. See spec §3.9.1 / Z8-D44.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProducerId(pub String);

impl ProducerId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProducerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ── Value, state, source, op (closed enums) ─────────────────────────────────

/// Locked-decision value. Closed enum; v1 covers the four shapes that
/// closed Failure B in the canonical aletheia thread plus `Choice` for
/// multiple-choice questions. Adding a variant requires a wire-version bump
/// per `spec-cross-cutting-wiring.md` §3.10.
///
/// `DecisionValue::Json` is **not** in v1 (deferred per spec §1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
#[non_exhaustive]
pub enum DecisionValue {
    /// Free-form text. Non-empty, ≤4096 bytes UTF-8 (Z8-D3b).
    String(String),
    Bool(bool),
    I64(i64),
    /// Filesystem path canonicalized per
    /// `spec-multi-repo-workspace-model.md` §3 (no `..`, no repeated `/`,
    /// no `~`, no trailing `/`). Z8-D4. The reducer in
    /// `caduceus-orchestrator` enforces this; this module only validates
    /// the surface invariants (non-empty, no `..`, no `~`).
    Path(String),
    /// Multiple-choice. `selected < options.len()` (Z8-D3a).
    Choice {
        options: Vec<String>,
        selected: u32,
    },
}

impl DecisionValue {
    /// Lightweight value-shape validation — does NOT do full path
    /// canonicalization (that lives in `caduceus-orchestrator`). Returns the
    /// stable error code that maps to a `DecisionRegisterError`.
    pub fn validate_shape(&self) -> Result<(), DecisionRegisterErrorCode> {
        match self {
            Self::String(s) => {
                if s.is_empty() {
                    return Err(DecisionRegisterErrorCode::InvalidValueShape);
                }
                if s.len() > 4096 {
                    return Err(DecisionRegisterErrorCode::InvalidValueShape);
                }
                Ok(())
            }
            Self::Bool(_) | Self::I64(_) => Ok(()),
            Self::Path(p) => {
                if p.is_empty() {
                    return Err(DecisionRegisterErrorCode::InvalidValueShape);
                }
                // Surface checks only — full canonicalization is the
                // reducer's job (Z8-D4 references the workspace-model spec).
                if p.contains("..") || p.starts_with('~') {
                    return Err(DecisionRegisterErrorCode::NonCanonicalPath);
                }
                Ok(())
            }
            Self::Choice { options, selected } => {
                if options.is_empty() {
                    return Err(DecisionRegisterErrorCode::InvalidValueShape);
                }
                if (*selected as usize) >= options.len() {
                    return Err(DecisionRegisterErrorCode::InvalidValueShape);
                }
                for opt in options {
                    if opt.is_empty() {
                        return Err(DecisionRegisterErrorCode::InvalidValueShape);
                    }
                }
                Ok(())
            }
        }
    }

    /// The discriminant kind, exposed so events that don't carry a value
    /// (e.g. `OpenQuestionPresented`) can describe the expected value
    /// shape.
    pub fn kind(&self) -> DecisionValueKind {
        match self {
            Self::String(_) => DecisionValueKind::String,
            Self::Bool(_) => DecisionValueKind::Bool,
            Self::I64(_) => DecisionValueKind::I64,
            Self::Path(_) => DecisionValueKind::Path,
            Self::Choice { .. } => DecisionValueKind::Choice,
        }
    }
}

/// Discriminant of [`DecisionValue`]. Carried by `OpenQuestionPresented` so
/// the user surface can render the right input control before any answer
/// has been chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum DecisionValueKind {
    String,
    Bool,
    I64,
    Path,
    Choice,
}

/// Lifecycle state. `Locked` ⇔ `current = Some(_)`; `Unlocked` ⇔
/// `current = None` (Z8-D17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionState {
    Locked,
    Unlocked,
}

/// Origin of the event. `System` is **not** a value — restore protocol
/// only injects context, it does not author decisions. See spec §3.4.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionSource {
    /// User-facing surface (zed plan panel, CLI flag) drove the event.
    User,
    /// Agent runner emitted the event.
    Agent,
}

/// Audit-log operation tag stored in each `DecisionRecord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionOp {
    Lock,
    Amend,
    Unlock,
    LockDenied,
}

// ── Audit record + entry ─────────────────────────────────────────────────────

/// One row in `DecisionEntry::history`. Append-only; never removed.
/// See spec §4.2 / Z8-D16.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub op: DecisionOp,
    /// `None` for `Unlock` and `LockDenied`; `Some(_)` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<DecisionValue>,
    pub source: DecisionSource,
    /// Wall-clock time of the mutation, ISO-8601 UTC string. The reducer
    /// projects monotonic instants to wall via the same machinery as
    /// `spec-orchestrator-status-snapshot.md` §3.1.
    pub locked_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    /// Required for `Amend`, `Unlock`, `LockDenied`. Optional for the initial
    /// `Lock`. Empty strings are rejected at the reducer (Z8-D9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Producer of this record — used for cross-process ordering tie-breaks
    /// (Z8-D44). Defaults to `"unknown"` for legacy records read off disk.
    #[serde(default = "default_producer_id")]
    pub producer_id: ProducerId,
}

fn default_producer_id() -> ProducerId {
    ProducerId::new("unknown")
}

/// One row in the `DecisionRegister`. Keyed by `DecisionId` in the register's
/// `BTreeMap`. See spec §3.2.1 / Z8-D16, Z8-D17.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub id: DecisionId,
    pub state: DecisionState,
    /// `Some(_)` iff `state == Locked`. Z8-D17.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<DecisionValue>,
    /// Append-only, oldest-first. Non-empty whenever the entry exists.
    pub history: Vec<DecisionRecord>,
    /// Provenance for agent-derived locks: the `DecisionId` whose value
    /// triggered this derivation. Replaces the v0 boolean `derived` flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<DecisionId>,
    /// Wall-clock ISO-8601 UTC of the first `Lock` event for this entry.
    pub locked_at: String,
    /// Wall-clock ISO-8601 UTC of the most recent `Amend`. `None` if never
    /// amended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_amended_at: Option<String>,
}

// ── OpenQuestion (drives the structural restore prong) ──────────────────────

/// One question the agent has presented to the user with a known
/// `DecisionId`. Lives in the per-thread open-question pool, removed when
/// the matching `DecisionLocked` lands. See spec §3.4.2 / Z8-D24.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenQuestion {
    pub id: DecisionId,
    pub prompt: String,
    pub kind: DecisionValueKind,
    /// Non-empty iff `kind == Choice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    pub presented_at: String,
    pub presented_by_execution_id: ExecutionId,
}

// ── Error code (closed enum) ─────────────────────────────────────────────────

/// Closed enum of reducer error codes. Surfaced as
/// `AgentEvent::DecisionRegisterError { code }` and as the `Err` variant of
/// IPC handler returns. Adding a code requires a spec amendment.
/// See spec §4.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DecisionRegisterErrorCode {
    /// `DecisionLocked` for an existing `Locked` id with a different value.
    /// Caller MUST emit `DecisionAmended` instead.
    ImplicitAmendForbidden,
    /// `DecisionAmended` for an unknown or `Unlocked` id.
    AmendOfUnlocked,
    /// `DecisionUnlocked` for an unknown or `Unlocked` id.
    UnlockOfUnlocked,
    /// `DecisionAmended.prior_value != entries[id].current`. Catches
    /// concurrent-amend races.
    StaleAmend,
    /// Agent attempted to silently override a `User`-sourced decision.
    AgentOverrodeUser,
    /// `DecisionAmended` / `DecisionUnlocked` with an empty `reason`.
    AmendWithoutReason,
    /// `DecisionId` violates §3.1.2 / Z8-D3.
    InvalidId,
    /// Choice.selected out of range, empty String, etc.
    InvalidValueShape,
    /// `DecisionValue::Path` violates the canonicalization rules.
    NonCanonicalPath,
    /// `EventSeq` regression within a single `BootId`. Fatal.
    OutOfOrder,
    /// An on-disk entry failed validation during RestoreProtocol step 2.
    QuarantineOnRestore,
}

impl DecisionRegisterErrorCode {
    /// Stable kebab-case wire string. Mirrors serde's `rename_all`
    /// representation; exposed for callers that want a string without
    /// going through serde.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ImplicitAmendForbidden => "implicit-amend-forbidden",
            Self::AmendOfUnlocked => "amend-of-unlocked",
            Self::UnlockOfUnlocked => "unlock-of-unlocked",
            Self::StaleAmend => "stale-amend",
            Self::AgentOverrodeUser => "agent-overrode-user",
            Self::AmendWithoutReason => "amend-without-reason",
            Self::InvalidId => "invalid-id",
            Self::InvalidValueShape => "invalid-value-shape",
            Self::NonCanonicalPath => "non-canonical-path",
            Self::OutOfOrder => "out-of-order",
            Self::QuarantineOnRestore => "quarantine-on-restore",
        }
    }
}

impl fmt::Display for DecisionRegisterErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Session-thread index (small wire type for §3.0.1.Z8-D43) ────────────────

/// Single-line UTF-8 record at `~/.caduceus/sessions/<session_id>/thread_id`
/// mapping a `SessionId` to the durable `ThreadId`. Atomic-rename written.
/// See spec §3.0.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionThreadIndex {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Z8-D3 / Z8-D2a: DecisionId discipline ──

    #[test]
    fn decision_id_accepts_canonical_examples() {
        for ok in [
            "a",
            "abc",
            "naming/framework",
            "path/scaffold-root",
            "student/substrate",
            "0x",
            "z9_y8-x7/w6",
        ] {
            DecisionId::new(ok).unwrap_or_else(|e| panic!("expected {ok:?} to parse, got {e}"));
        }
    }

    #[test]
    fn decision_id_rejects_uppercase() {
        assert!(DecisionId::new("Naming/framework").is_err());
        assert!(DecisionId::new("namE").is_err());
    }

    #[test]
    fn decision_id_rejects_disallowed_chars() {
        for bad in [
            "naming.framework",
            "name space",
            "a:b",
            "!root",
            "name?",
            "тест",
        ] {
            assert!(DecisionId::new(bad).is_err(), "expected reject: {bad:?}");
        }
    }

    #[test]
    fn decision_id_rejects_empty_and_oversize() {
        assert!(DecisionId::new("").is_err());
        let big = "a".repeat(129);
        assert!(DecisionId::new(big).is_err());
        // Exactly 128 is OK.
        let edge = "a".repeat(128);
        DecisionId::new(edge).unwrap();
    }

    #[test]
    fn decision_id_rejects_first_char_violations() {
        assert!(DecisionId::new("/abc").is_err());
        assert!(DecisionId::new("-abc").is_err());
        assert!(DecisionId::new("_abc").is_err());
    }

    // ── Z8-D3a / Z8-D3b: value-shape ──

    #[test]
    fn value_string_validates_non_empty_and_size() {
        assert!(DecisionValue::String(String::new())
            .validate_shape()
            .is_err());
        assert!(DecisionValue::String("x".into()).validate_shape().is_ok());
        let big = "x".repeat(4097);
        assert!(DecisionValue::String(big).validate_shape().is_err());
    }

    #[test]
    fn value_choice_validates_selected_index() {
        let v = DecisionValue::Choice {
            options: vec!["a".into(), "b".into()],
            selected: 0,
        };
        assert!(v.validate_shape().is_ok());

        let oob = DecisionValue::Choice {
            options: vec!["a".into()],
            selected: 1,
        };
        assert!(oob.validate_shape().is_err());

        let empty = DecisionValue::Choice {
            options: vec![],
            selected: 0,
        };
        assert!(empty.validate_shape().is_err());

        let blank_opt = DecisionValue::Choice {
            options: vec!["a".into(), String::new()],
            selected: 0,
        };
        assert!(blank_opt.validate_shape().is_err());
    }

    #[test]
    fn value_path_rejects_traversal_and_tilde() {
        assert!(DecisionValue::Path("/Users/alex/aletheia".into())
            .validate_shape()
            .is_ok());
        assert!(DecisionValue::Path("/etc/../etc/passwd".into())
            .validate_shape()
            .is_err());
        assert!(DecisionValue::Path("~/aletheia".into())
            .validate_shape()
            .is_err());
        assert!(DecisionValue::Path(String::new()).validate_shape().is_err());
    }

    #[test]
    fn value_kind_round_trips() {
        let s = serde_json::to_string(&DecisionValueKind::Choice).unwrap();
        assert_eq!(s, "\"choice\"");
        let back: DecisionValueKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, DecisionValueKind::Choice);
    }

    // ── serde tag/content layout for DecisionValue ──

    #[test]
    fn decision_value_uses_kind_value_tagging() {
        let v = DecisionValue::Bool(true);
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, r#"{"kind":"bool","value":true}"#);

        let back: DecisionValue = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn decision_value_choice_round_trip() {
        let v = DecisionValue::Choice {
            options: vec!["python".into(), "rust".into(), "ts".into()],
            selected: 0,
        };
        let s = serde_json::to_string(&v).unwrap();
        let back: DecisionValue = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn decision_entry_round_trip() {
        let id = DecisionId::new("naming/framework").unwrap();
        let value = DecisionValue::String("Aletheia".into());
        let rec = DecisionRecord {
            op: DecisionOp::Lock,
            value: Some(value.clone()),
            source: DecisionSource::User,
            locked_at: "2026-05-04T08:00:00Z".into(),
            request_id: None,
            reason: None,
            producer_id: ProducerId::new("agent-runner-claude"),
        };
        let entry = DecisionEntry {
            id: id.clone(),
            state: DecisionState::Locked,
            current: Some(value),
            history: vec![rec],
            derived_from: None,
            locked_at: "2026-05-04T08:00:00Z".into(),
            last_amended_at: None,
        };
        let s = serde_json::to_string(&entry).unwrap();
        let back: DecisionEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn decision_record_default_producer_id_on_legacy_read() {
        let legacy_json = r#"{
            "op": "lock",
            "value": {"kind":"bool","value":true},
            "source": "agent",
            "locked_at": "2026-05-04T08:00:00Z"
        }"#;
        let rec: DecisionRecord = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(rec.producer_id.as_str(), "unknown");
    }

    // ── Error codes ──

    #[test]
    fn error_code_kebab_case_wire() {
        let cases = [
            (
                DecisionRegisterErrorCode::ImplicitAmendForbidden,
                "implicit-amend-forbidden",
            ),
            (
                DecisionRegisterErrorCode::AmendOfUnlocked,
                "amend-of-unlocked",
            ),
            (
                DecisionRegisterErrorCode::UnlockOfUnlocked,
                "unlock-of-unlocked",
            ),
            (DecisionRegisterErrorCode::StaleAmend, "stale-amend"),
            (
                DecisionRegisterErrorCode::AgentOverrodeUser,
                "agent-overrode-user",
            ),
            (
                DecisionRegisterErrorCode::AmendWithoutReason,
                "amend-without-reason",
            ),
            (DecisionRegisterErrorCode::InvalidId, "invalid-id"),
            (
                DecisionRegisterErrorCode::InvalidValueShape,
                "invalid-value-shape",
            ),
            (
                DecisionRegisterErrorCode::NonCanonicalPath,
                "non-canonical-path",
            ),
            (DecisionRegisterErrorCode::OutOfOrder, "out-of-order"),
            (
                DecisionRegisterErrorCode::QuarantineOnRestore,
                "quarantine-on-restore",
            ),
        ];
        for (code, want) in cases {
            assert_eq!(code.as_str(), want);
            assert_eq!(code.to_string(), want);
            // Serde mirrors the wire string.
            let s = serde_json::to_string(&code).unwrap();
            assert_eq!(s, format!("\"{}\"", want));
            let back: DecisionRegisterErrorCode = serde_json::from_str(&s).unwrap();
            assert_eq!(back, code);
        }
    }

    // ── ThreadId / ProducerId ──

    #[test]
    fn thread_id_round_trips() {
        let t = ThreadId::new();
        let s = serde_json::to_string(&t).unwrap();
        let back: ThreadId = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn producer_id_orders_lexicographically() {
        let mut producers = vec![
            ProducerId::new("zed-cli"),
            ProducerId::new("agent-runner-claude"),
            ProducerId::new("agent-runner-gpt"),
        ];
        producers.sort();
        let names: Vec<&str> = producers.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            names,
            vec!["agent-runner-claude", "agent-runner-gpt", "zed-cli"]
        );
    }
}
