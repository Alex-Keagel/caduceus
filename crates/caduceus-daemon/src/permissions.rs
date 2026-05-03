//! Permission envelope + resolver + elevation flow + denial routing
//! (pm01 + pm02 + pm03 + pm04 + pm05).
//!
//! Per the implementation DAG, this module ships the daemon's
//! permission subsystem as defined by `spec-m-permissions.md`.
//! Concerns:
//!
//! - **`pm01`** — `PermissionEnvelope`: scope + capability + bindings.
//! - **`pm02`** — `PermissionResolver`: pure-function (envelope,
//!   request) -> decision.
//! - **`pm03`** — elevation flow integration with `ru21` runner-side
//!   forwarder (already in `runner_extras`); this module supplies the
//!   resolver-side decision.
//! - **`pm04`** — denial routing: `Switched` / `Denied` /
//!   `ProfileSwitchPending` classifier per the prior caduceus-zed
//!   ST8 PR-B series.
//! - **`pm05`** — classify denied tool requests by category (write,
//!   exec, network, sensitive).

use crate::runner_extras::ElevationDecision;
use serde::{Deserialize, Serialize};

/// Capability classes — coarse buckets that decide which envelope rule
/// applies.  Spec m-permissions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityClass {
    /// Read-only access (file read, list).
    Read,
    /// Write access to filesystem.
    Write,
    /// Execute external commands / shell.
    Exec,
    /// Network egress.
    Network,
    /// Sensitive operations: secrets, credentials, system config.
    Sensitive,
}

/// A specific capability the agent is requesting.  Composed of a class
/// + a string identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Capability {
    pub class: CapabilityClass,
    /// Specific id (e.g., `"fs.write"`, `"network.https"`).
    pub id: String,
}

impl Capability {
    pub fn new(class: CapabilityClass, id: impl Into<String>) -> Self {
        Self {
            class,
            id: id.into(),
        }
    }
}

/// A request from the runner for elevation to a specific capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRequest {
    pub capability: Capability,
    pub reason: String,
}

/// Default verdict for a capability when it is not explicitly listed
/// in the envelope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DefaultPolicy {
    Deny,
    Allow,
    PromptUser,
}

/// Permission envelope — the v1 policy shape.  Spec m-permissions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionEnvelope {
    /// Profile name (e.g., "plan", "research", "act", "autopilot").
    pub profile: String,
    /// Explicitly allowed capabilities (no prompt).
    pub allow: Vec<Capability>,
    /// Explicitly denied capabilities (no prompt).
    pub deny: Vec<Capability>,
    /// Capabilities that require user prompt.
    pub prompt: Vec<Capability>,
    /// Default policy for unlisted capabilities.
    pub default: DefaultPolicy,
}

impl PermissionEnvelope {
    /// V1 read-only "plan" preset.  No write, no exec, no network.
    pub fn preset_plan() -> Self {
        Self {
            profile: "plan".into(),
            allow: vec![Capability::new(CapabilityClass::Read, "fs.read")],
            deny: vec![
                Capability::new(CapabilityClass::Write, "fs.write"),
                Capability::new(CapabilityClass::Exec, "shell.exec"),
                Capability::new(CapabilityClass::Network, "network.egress"),
            ],
            prompt: vec![],
            default: DefaultPolicy::Deny,
        }
    }

    /// V1 "act" preset: writes + exec allowed; network + sensitive prompts.
    pub fn preset_act() -> Self {
        Self {
            profile: "act".into(),
            allow: vec![
                Capability::new(CapabilityClass::Read, "fs.read"),
                Capability::new(CapabilityClass::Write, "fs.write"),
                Capability::new(CapabilityClass::Exec, "shell.exec"),
            ],
            deny: vec![],
            prompt: vec![
                Capability::new(CapabilityClass::Network, "network.egress"),
                Capability::new(CapabilityClass::Sensitive, "secrets.read"),
            ],
            default: DefaultPolicy::PromptUser,
        }
    }

    /// V1 "autopilot" preset: writes + exec + network allowed; sensitive prompts.
    pub fn preset_autopilot() -> Self {
        Self {
            profile: "autopilot".into(),
            allow: vec![
                Capability::new(CapabilityClass::Read, "fs.read"),
                Capability::new(CapabilityClass::Write, "fs.write"),
                Capability::new(CapabilityClass::Exec, "shell.exec"),
                Capability::new(CapabilityClass::Network, "network.egress"),
            ],
            deny: vec![],
            prompt: vec![Capability::new(CapabilityClass::Sensitive, "secrets.read")],
            default: DefaultPolicy::PromptUser,
        }
    }
}

/// Resolver — pure function (envelope, request) -> ElevationDecision.
pub fn resolve(envelope: &PermissionEnvelope, request: &PermissionRequest) -> ElevationDecision {
    // Explicit deny overrides everything.
    if envelope.deny.iter().any(|c| c == &request.capability) {
        return ElevationDecision::Deny;
    }
    // Explicit allow.
    if envelope.allow.iter().any(|c| c == &request.capability) {
        return ElevationDecision::Allow;
    }
    // Explicit prompt.
    if envelope.prompt.iter().any(|c| c == &request.capability) {
        return ElevationDecision::PromptUser;
    }
    // Default.
    match envelope.default {
        DefaultPolicy::Deny => ElevationDecision::Deny,
        DefaultPolicy::Allow => ElevationDecision::Allow,
        DefaultPolicy::PromptUser => ElevationDecision::PromptUser,
    }
}

// ───────────────────────── pm04 — denial routing ──────────────────────

/// Outcome categories for denied tool requests.  Cross-link to
/// caduceus-zed ST8 PR-B series.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DenialOutcome {
    /// Request denied; no profile switch will help.
    Denied,
    /// Tool was switched to an equivalent that requires no elevation
    /// (e.g., write -> patch via approved tool).
    Switched,
    /// Profile switch is pending user confirmation; the tool MAY be
    /// allowed under the new profile.
    ProfileSwitchPending,
}

/// Classify the resolver's `Deny` decision into a routing outcome.
/// Spec m-permissions denial-routing: if there exists an alternative
/// allowing profile, route to `ProfileSwitchPending`; if the tool has
/// a switch-equivalent in the current profile, route to `Switched`;
/// otherwise `Denied`.
///
/// V1 simplification: this function takes pre-computed booleans; the
/// real lookup logic lives in the workflow loader.
pub fn route_denial(has_switch_equivalent: bool, has_profile_alternative: bool) -> DenialOutcome {
    if has_switch_equivalent {
        DenialOutcome::Switched
    } else if has_profile_alternative {
        DenialOutcome::ProfileSwitchPending
    } else {
        DenialOutcome::Denied
    }
}

// ───────────────────────── pm05 — denial classifier ───────────────────

/// Categorize a denied capability for diagnostics + UX.  Spec
/// m-permissions classifier maps capability classes to user-facing
/// categories.
pub fn classify_denial(capability: &Capability) -> &'static str {
    match capability.class {
        CapabilityClass::Read => "read",
        CapabilityClass::Write => "write",
        CapabilityClass::Exec => "exec",
        CapabilityClass::Network => "network",
        CapabilityClass::Sensitive => "sensitive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(class: CapabilityClass, id: &str) -> PermissionRequest {
        PermissionRequest {
            capability: Capability::new(class, id),
            reason: "test".into(),
        }
    }

    // ─── Envelope presets ────────────────────────────────────────────

    #[test]
    fn plan_preset_denies_write() {
        let env = PermissionEnvelope::preset_plan();
        let r = resolve(&env, &req(CapabilityClass::Write, "fs.write"));
        assert_eq!(r, ElevationDecision::Deny);
    }

    #[test]
    fn plan_preset_denies_exec() {
        let env = PermissionEnvelope::preset_plan();
        let r = resolve(&env, &req(CapabilityClass::Exec, "shell.exec"));
        assert_eq!(r, ElevationDecision::Deny);
    }

    #[test]
    fn plan_preset_denies_network() {
        let env = PermissionEnvelope::preset_plan();
        let r = resolve(&env, &req(CapabilityClass::Network, "network.egress"));
        assert_eq!(r, ElevationDecision::Deny);
    }

    #[test]
    fn plan_preset_allows_fs_read() {
        let env = PermissionEnvelope::preset_plan();
        let r = resolve(&env, &req(CapabilityClass::Read, "fs.read"));
        assert_eq!(r, ElevationDecision::Allow);
    }

    #[test]
    fn act_preset_allows_write_and_exec_prompts_network() {
        let env = PermissionEnvelope::preset_act();
        assert_eq!(
            resolve(&env, &req(CapabilityClass::Write, "fs.write")),
            ElevationDecision::Allow
        );
        assert_eq!(
            resolve(&env, &req(CapabilityClass::Exec, "shell.exec")),
            ElevationDecision::Allow
        );
        assert_eq!(
            resolve(&env, &req(CapabilityClass::Network, "network.egress")),
            ElevationDecision::PromptUser
        );
    }

    #[test]
    fn autopilot_preset_allows_network() {
        let env = PermissionEnvelope::preset_autopilot();
        assert_eq!(
            resolve(&env, &req(CapabilityClass::Network, "network.egress")),
            ElevationDecision::Allow
        );
    }

    #[test]
    fn explicit_deny_overrides_default_allow() {
        let env = PermissionEnvelope {
            profile: "test".into(),
            allow: vec![],
            deny: vec![Capability::new(CapabilityClass::Write, "fs.write")],
            prompt: vec![],
            default: DefaultPolicy::Allow,
        };
        let r = resolve(&env, &req(CapabilityClass::Write, "fs.write"));
        assert_eq!(r, ElevationDecision::Deny);
    }

    #[test]
    fn unlisted_capability_falls_to_default_policy() {
        let env = PermissionEnvelope {
            profile: "test".into(),
            allow: vec![],
            deny: vec![],
            prompt: vec![],
            default: DefaultPolicy::PromptUser,
        };
        let r = resolve(&env, &req(CapabilityClass::Network, "network.egress"));
        assert_eq!(r, ElevationDecision::PromptUser);
    }

    // ─── pm04 denial routing ─────────────────────────────────────────

    #[test]
    fn denial_routes_to_switched_when_equivalent_exists() {
        let r = route_denial(true, false);
        assert_eq!(r, DenialOutcome::Switched);
    }

    #[test]
    fn denial_routes_to_profile_switch_pending_when_alternative_exists() {
        let r = route_denial(false, true);
        assert_eq!(r, DenialOutcome::ProfileSwitchPending);
    }

    #[test]
    fn denial_routes_to_denied_when_no_alternative() {
        let r = route_denial(false, false);
        assert_eq!(r, DenialOutcome::Denied);
    }

    #[test]
    fn switched_takes_precedence_over_profile_switch() {
        let r = route_denial(true, true);
        assert_eq!(r, DenialOutcome::Switched);
    }

    // ─── pm05 classifier ──────────────────────────────────────────────

    #[test]
    fn classify_denial_buckets_by_class() {
        assert_eq!(
            classify_denial(&Capability::new(CapabilityClass::Write, "fs.write")),
            "write"
        );
        assert_eq!(
            classify_denial(&Capability::new(CapabilityClass::Exec, "shell.exec")),
            "exec"
        );
        assert_eq!(
            classify_denial(&Capability::new(CapabilityClass::Network, "network.egress")),
            "network"
        );
        assert_eq!(
            classify_denial(&Capability::new(CapabilityClass::Sensitive, "secrets.read")),
            "sensitive"
        );
        assert_eq!(
            classify_denial(&Capability::new(CapabilityClass::Read, "fs.read")),
            "read"
        );
    }

    #[test]
    fn envelope_serialize_round_trip() {
        let env = PermissionEnvelope::preset_act();
        let s = serde_json::to_string(&env).unwrap();
        let back: PermissionEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(env, back);
    }
}
