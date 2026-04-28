//! Grant composition for scope-expansion (ST8 — PR1 of 3).
//!
//! When the agent attempts an out-of-envelope action, the orchestrator emits
//! [`PermissionEvent::ScopeExpansionRequested`]. The user (or an automated
//! policy) may then respond with a *grant* — an updated envelope that widens
//! the agent's capabilities just enough to complete the requested action.
//!
//! This module defines the **contract** that all grants must satisfy:
//!
//! 1. **Append-only / monotonic widening.** A grant can only widen the
//!    envelope. The grant validator rejects any update that *narrows* a
//!    capability the agent already has. Widening is defined per-field
//!    (see [`validate_widening`]).
//!
//! 2. **Idempotent.** Applying the same grant twice produces the same
//!    envelope. Re-issuing a grant that's already been applied is a no-op,
//!    not an error.
//!
//! 3. **Security-flag invariance.** Some fields (e.g.
//!    `treat_tool_output_as_untrusted`, `fanout_policy`, `scope`) are
//!    *invariant* across grants — they can only be set at envelope creation.
//!    Grants that try to flip them are rejected. This prevents a grant from
//!    inadvertently weakening prompt-injection guards or changing the
//!    envelope's lifecycle scope.
//!
//! 4. **Deny-lists are invariant under grants.** Adding entries to a deny
//!    list (path deny, network `host_deny`, exec `always_deny_substrings`)
//!    *narrows* what was previously allowed: a path matched by both
//!    `allow` and a new `deny` entry flips from Allow to Deny. So the
//!    validator requires deny-lists to be **equal** between prev and next.
//!    Removing entries is rejected (loosens safety guards); adding entries
//!    is rejected (revokes previously-granted capability). Modifying
//!    safety guards belongs in envelope construction, not in grants.
//!
//! ## Usage
//!
//! Consumers (notably the orchestrator's harness) hold an
//! [`EnvelopeMutator`] and call [`EnvelopeMutator::apply_grant`] when a
//! `ScopeExpansionGranted` event arrives. The default implementation
//! ([`DefaultEnvelopeMutator`]) enforces the contract above and returns the
//! validated next envelope, or a [`GrantValidationError`] explaining which
//! invariant was violated.
//!
//! Future PRs will add concurrent grant arbitration in the orchestrator
//! (PR-2) and the user-facing UI surface in zed (PR-3). This crate stays
//! free of those concerns: the mutator is a pure function over envelopes.

use crate::envelope::{
    ApprovalCadence, EnvelopeScope, ExecPolicy, FanoutPolicy, NetworkPolicy, PathAllowlist,
    PermissionEnvelope,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The outcome of an in-flight scope-expansion request as observed by the
/// orchestrator. `Granted` carries the proposed updated envelope; `Denied`
/// carries a user-facing reason; `Timeout` indicates the grant deadline
/// elapsed before any response arrived.
///
/// Used by future PRs (PR-2) to drive the deny-commit pause; included in
/// PR-1 so the contract for "what does a grant look like" is in one place
/// and tested in isolation.
///
/// # Over-grant warning
///
/// Carrying a full envelope (rather than a delta scoped to the pending
/// [`crate::envelope::ExpansionDelta`]) lets a buggy or malicious UI
/// approve *more* than the agent asked for. The widening validator alone
/// cannot detect this — it only checks monotonicity, not "did the grant
/// match the request". PR-2 (orchestrator) MUST re-validate the granted
/// envelope against the pending `ExpansionDelta` (e.g. require the
/// granted envelope to widen `current` only along the requested
/// capability/resource axis). Tracking note: see ST8 deferral doc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum GrantOutcome {
    /// User approved; this is the proposed updated envelope. Must be
    /// validated by an [`EnvelopeMutator`] before being applied — the
    /// orchestrator should not trust the proposal verbatim.
    Granted { updated: PermissionEnvelope },
    /// User explicitly denied with a reason. The original deny commits
    /// to the transcript.
    Denied { reason: String },
    /// No response arrived within the grant deadline. The original deny
    /// commits to the transcript.
    Timeout,
}

/// Errors a grant validator can raise. Each variant identifies a specific
/// invariant violation; consumers can use these for telemetry and for
/// surfacing user-facing messages.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GrantValidationError {
    #[error("scope must remain {prev:?}, grant attempted to change to {next:?}")]
    ScopeChanged {
        prev: EnvelopeScope,
        next: EnvelopeScope,
    },

    #[error(
        "treat_tool_output_as_untrusted is invariant across grants \
         (prev={prev}, next={next})"
    )]
    UntrustedOutputFlagChanged { prev: bool, next: bool },

    #[error("fanout_policy is invariant across grants (prev={prev:?}, next={next:?})")]
    FanoutPolicyChanged {
        prev: FanoutPolicy,
        next: FanoutPolicy,
    },

    #[error("read allowlist removed entries that were previously allowed: {removed:?}")]
    ReadAllowShrunk { removed: Vec<String> },

    #[error("write allowlist removed entries that were previously allowed: {removed:?}")]
    WriteAllowShrunk { removed: Vec<String> },

    #[error(
        "read deny list changed (deny is invariant under grants): prev={prev:?}, next={next:?}"
    )]
    ReadDenyChanged {
        prev: Vec<String>,
        next: Vec<String>,
    },

    #[error(
        "write deny list changed (deny is invariant under grants): prev={prev:?}, next={next:?}"
    )]
    WriteDenyChanged {
        prev: Vec<String>,
        next: Vec<String>,
    },

    #[error("read intercept_denied flipped from true to false (cannot relax in grant)")]
    ReadInterceptRelaxed,

    #[error("write intercept_denied flipped from true to false (cannot relax in grant)")]
    WriteInterceptRelaxed,

    #[error(
        "network was enabled and grant disables it; \
         grants cannot narrow capabilities"
    )]
    NetworkDisabledByGrant,

    #[error(
        "network host_deny changed (deny is invariant under grants): \
         prev={prev:?}, next={next:?}"
    )]
    NetworkHostDenyChanged {
        prev: Vec<String>,
        next: Vec<String>,
    },

    #[error(
        "network host_allow narrowed: prev allowed {prev:?}, next would only allow \
         {next:?} (grants cannot narrow)"
    )]
    NetworkHostAllowNarrowed {
        prev: Vec<String>,
        next: Vec<String>,
    },

    #[error("exec was enabled and grant disables it; grants cannot narrow capabilities")]
    ExecDisabledByGrant,

    #[error(
        "exec always_deny_substrings changed (safety guards are invariant under grants): \
         prev={prev:?}, next={next:?}"
    )]
    ExecAlwaysDenyChanged {
        prev: Vec<String>,
        next: Vec<String>,
    },

    #[error(
        "approval_cadence tightened: prev={prev:?}, next={next:?} \
         (grants can only relax cadence)"
    )]
    CadenceTightened {
        prev: ApprovalCadence,
        next: ApprovalCadence,
    },

    #[error("skill_budget shrunk: prev={prev}, next={next} (grants cannot narrow)")]
    SkillBudgetShrunk { prev: usize, next: usize },
}

/// Trait for composing grants onto an existing envelope.
///
/// Implementations must satisfy the four contract points documented at the
/// module level. The default implementation ([`DefaultEnvelopeMutator`])
/// enforces the contract; alternative implementations may layer additional
/// policies (e.g. logging, dry-run, role-based caps) but must not relax the
/// widening invariants.
///
/// `apply_grant` is a pure function — it does not perform I/O, does not
/// mutate `current` in place, and is safe to call from any context.
pub trait EnvelopeMutator: Send + Sync {
    /// Apply `proposed` as a grant on top of `current`, returning the
    /// validated next envelope.
    ///
    /// - Returns `Ok(proposed.clone())` (after validation) if the grant
    ///   widens or equals `current`.
    /// - Returns `Err(GrantValidationError)` identifying the first
    ///   invariant violation otherwise. Validation short-circuits on the
    ///   first error; callers wanting the full diff should fix and retry.
    fn apply_grant(
        &self,
        current: &PermissionEnvelope,
        proposed: &PermissionEnvelope,
    ) -> Result<PermissionEnvelope, GrantValidationError>;
}

/// Default mutator: enforces the widening contract and returns the
/// proposed envelope as-is on success.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultEnvelopeMutator;

impl EnvelopeMutator for DefaultEnvelopeMutator {
    fn apply_grant(
        &self,
        current: &PermissionEnvelope,
        proposed: &PermissionEnvelope,
    ) -> Result<PermissionEnvelope, GrantValidationError> {
        validate_widening(current, proposed)?;
        Ok(proposed.clone())
    }
}

// ── ST8 PR-3D — widening helper for UI Allow path ──────────────────────────

/// Errors raised by [`propose_widened_envelope`] when the requested
/// `(capability, resource)` cannot be mapped onto a sensible widening of
/// `current`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WidenProposalError {
    /// The capability string is not one of the four
    /// [`crate::ExpansionCapability`] tags emitted by the orchestrator
    /// preflight (`"read"`, `"write"`, `"network"`, `"exec"`).
    #[error("unknown capability '{capability}' — expected one of: read, write, network, exec")]
    UnknownCapability { capability: String },

    /// The resource is empty / whitespace-only. The orchestrator emits a
    /// concrete path / host / command; an empty resource indicates a
    /// caller bug, not a user choice.
    #[error("resource for capability '{capability}' is empty")]
    EmptyResource { capability: String },
}

/// ST8 PR-3D — produce a minimally-widened envelope that approves the
/// pending `(capability, resource)` axis only.
///
/// Designed for the UI Allow path: the picker (or any approval bot) holds
/// the *current* envelope and the `(capability, resource)` carried by the
/// in-flight scope-expansion request, and needs to construct a
/// [`PermissionEnvelope`] suitable for [`GrantOutcome::Granted`] without
/// re-implementing the per-capability widening rules.
///
/// ## Capability mapping
///
/// | `capability` (string) | Effect on a clone of `current`                                          |
/// |-----------------------|-------------------------------------------------------------------------|
/// | `"read"`              | Append `resource` to `read.allow` if absent (de-duped).                |
/// | `"write"`             | Append `resource` to `write.allow` if absent (de-duped).               |
/// | `"network"`           | Force `network.enabled = true`. If `host_allow` is non-empty and does not already contain `resource`, append it. **Empty `host_allow` (open-all) stays empty** — appending would *narrow* the policy. |
/// | `"exec"`              | Force `exec.enabled = true`. (Exec has no resource allowlist; the deny-substrings guard remains untouched, satisfying the grant's safety-flag invariance.) |
///
/// ## What this *doesn't* do
///
/// - **No deny-list mutations.** Deny lists are invariant under grants
///   (see [`validate_widening`]). The helper never touches them.
/// - **No safety-flag flips.** `treat_tool_output_as_untrusted`,
///   `fanout_policy`, `scope`, `approval_cadence`, `skill_budget`,
///   `intercept_denied`, and `always_deny_substrings` are all preserved
///   verbatim from `current`.
/// - **No least-privilege enforcement beyond the requested axis.** If
///   `current` already grants something broader than what the user is
///   approving (e.g. an empty `host_allow` meaning "all hosts"), the
///   helper does not narrow it. The grant contract is monotonic
///   widening; this helper is the smallest such widening for a single
///   axis.
///
/// ## Round-trip with [`validate_widening`]
///
/// The output is guaranteed to satisfy
/// `validate_widening(current, &proposed).is_ok()` — by construction, the
/// helper only touches fields that the validator allows to widen, and
/// only in the widening direction.
///
/// Callers can pass the result to [`GrantOutcome::Granted`] and submit it
/// via the orchestrator's `submit_grant`. The orchestrator independently
/// re-validates against the in-flight `ExpansionDelta`, so a malicious
/// caller cannot escape the over-grant defense documented on
/// [`GrantOutcome`].
pub fn propose_widened_envelope(
    current: &PermissionEnvelope,
    capability: &str,
    resource: &str,
) -> Result<PermissionEnvelope, WidenProposalError> {
    let cap = capability.trim();
    if resource.trim().is_empty() {
        return Err(WidenProposalError::EmptyResource {
            capability: cap.to_string(),
        });
    }

    let mut next = current.clone();
    match cap {
        "read" => {
            push_unique_path(&mut next.read.allow, resource);
        }
        "write" => {
            push_unique_path(&mut next.write.allow, resource);
        }
        "network" => {
            next.network.enabled = true;
            push_unique_host(&mut next.network.host_allow, resource);
        }
        "exec" => {
            next.exec.enabled = true;
            // Exec has no per-command allowlist — `resource` (the command
            // string) is informational. The always_deny_substrings guard
            // still applies and is preserved by the field-level clone.
        }
        _ => {
            return Err(WidenProposalError::UnknownCapability {
                capability: cap.to_string(),
            });
        }
    }
    Ok(next)
}

/// Path allowlists: empty means "deny all" (per [`crate::PathAllowlist`]
/// `check()` semantics). Appending is always safe — going from `[]` to
/// `[x]` widens, and the validator accepts strict superset growth.
fn push_unique_path(list: &mut Vec<String>, value: &str) {
    if list.iter().any(|x| x == value) {
        return;
    }
    list.push(value.to_string());
}

/// Network host allow: empty means "all hosts allowed" (per
/// [`crate::NetworkPolicy::check`]). Appending to an empty list would
/// *narrow* the policy from "any host" to a single host — that violates
/// the widening contract and the validator would reject it. Only append
/// when the list is already non-empty (an explicit allowlist).
fn push_unique_host(list: &mut Vec<String>, value: &str) {
    if list.is_empty() {
        return;
    }
    if list.iter().any(|x| x == value) {
        return;
    }
    list.push(value.to_string());
}

/// Validates that `next` widens (or equals) `prev` per the grant contract.
///
/// Field-by-field semantics:
///
/// | Field                              | Rule                                              |
/// |------------------------------------|---------------------------------------------------|
/// | `scope`                            | Must equal `prev.scope` (invariant).              |
/// | `treat_tool_output_as_untrusted`   | Must equal `prev` (invariant).                    |
/// | `fanout_policy`                    | Must equal `prev` (invariant).                    |
/// | `read.allow` / `write.allow`       | Must be a superset of `prev.allow`.               |
/// | `read.deny` / `write.deny`         | Must equal `prev.deny` (invariant — deny growth narrows). |
/// | `read.intercept_denied` etc.      | Cannot flip true→false (relaxing intercept narrows the agent's view of denials). |
/// | `network.enabled`                  | Cannot flip true→false.                           |
/// | `network.host_deny`                | Must equal `prev` (invariant — deny growth narrows). |
/// | `network.host_allow`               | Must "widen": empty stays empty, or grows.        |
/// | `exec.enabled`                     | Cannot flip true→false.                           |
/// | `exec.always_deny_substrings`      | Must equal `prev` (invariant — guards stay).      |
/// | `approval_cadence`                 | `PerMajorStep → None` allowed; reverse rejected.  |
/// | `skill_budget`                     | Cannot shrink.                                    |
///
/// Note: the grant validator does **not** enforce that the grant is
/// minimally-widening. A grant that goes from `act` → `autopilot` (much
/// wider) is permitted. Callers that want to enforce least-privilege grants
/// should validate at a higher layer; this validator's contract is purely
/// monotonicity along the *capability* dimensions, with safety guards
/// (deny-lists) held invariant.
pub fn validate_widening(
    prev: &PermissionEnvelope,
    next: &PermissionEnvelope,
) -> Result<(), GrantValidationError> {
    // Invariants ───────────────────────────────────────────────────────────
    if prev.scope != next.scope {
        return Err(GrantValidationError::ScopeChanged {
            prev: prev.scope,
            next: next.scope,
        });
    }
    if prev.treat_tool_output_as_untrusted != next.treat_tool_output_as_untrusted {
        return Err(GrantValidationError::UntrustedOutputFlagChanged {
            prev: prev.treat_tool_output_as_untrusted,
            next: next.treat_tool_output_as_untrusted,
        });
    }
    if prev.fanout_policy != next.fanout_policy {
        return Err(GrantValidationError::FanoutPolicyChanged {
            prev: prev.fanout_policy,
            next: next.fanout_policy,
        });
    }

    // Path allowlists ──────────────────────────────────────────────────────
    validate_path_widening(&prev.read, &next.read, /* is_write */ false)?;
    validate_path_widening(&prev.write, &next.write, /* is_write */ true)?;

    // Network ──────────────────────────────────────────────────────────────
    validate_network_widening(&prev.network, &next.network)?;

    // Exec ─────────────────────────────────────────────────────────────────
    validate_exec_widening(&prev.exec, &next.exec)?;

    // Cadence ──────────────────────────────────────────────────────────────
    if !cadence_widens(prev.approval_cadence, next.approval_cadence) {
        return Err(GrantValidationError::CadenceTightened {
            prev: prev.approval_cadence,
            next: next.approval_cadence,
        });
    }

    // Skill budget ─────────────────────────────────────────────────────────
    if next.skill_budget < prev.skill_budget {
        return Err(GrantValidationError::SkillBudgetShrunk {
            prev: prev.skill_budget,
            next: next.skill_budget,
        });
    }

    Ok(())
}

fn validate_path_widening(
    prev: &PathAllowlist,
    next: &PathAllowlist,
    is_write: bool,
) -> Result<(), GrantValidationError> {
    let removed_allow: Vec<String> = prev
        .allow
        .iter()
        .filter(|p| !next.allow.contains(p))
        .cloned()
        .collect();
    if !removed_allow.is_empty() {
        return Err(if is_write {
            GrantValidationError::WriteAllowShrunk {
                removed: removed_allow,
            }
        } else {
            GrantValidationError::ReadAllowShrunk {
                removed: removed_allow,
            }
        });
    }

    // Deny lists are *invariant* under grants. Adding entries narrows
    // (a path matched by both allow and a new deny entry flips Allow → Deny);
    // removing entries loosens safety guards. Either belongs in envelope
    // construction, not in a grant.
    if prev.deny != next.deny {
        return Err(if is_write {
            GrantValidationError::WriteDenyChanged {
                prev: prev.deny.clone(),
                next: next.deny.clone(),
            }
        } else {
            GrantValidationError::ReadDenyChanged {
                prev: prev.deny.clone(),
                next: next.deny.clone(),
            }
        });
    }

    if prev.intercept_denied && !next.intercept_denied {
        return Err(if is_write {
            GrantValidationError::WriteInterceptRelaxed
        } else {
            GrantValidationError::ReadInterceptRelaxed
        });
    }

    Ok(())
}

fn validate_network_widening(
    prev: &NetworkPolicy,
    next: &NetworkPolicy,
) -> Result<(), GrantValidationError> {
    if prev.enabled && !next.enabled {
        return Err(GrantValidationError::NetworkDisabledByGrant);
    }

    // host_deny is invariant under grants — adding entries narrows host_allow
    // (a host listed in both flips from Allow to Deny); removing entries
    // loosens safety guards.
    if prev.host_deny != next.host_deny {
        return Err(GrantValidationError::NetworkHostDenyChanged {
            prev: prev.host_deny.clone(),
            next: next.host_deny.clone(),
        });
    }

    // host_allow has special "empty == open" semantics (see NetworkPolicy::check):
    // - prev empty → all hosts allowed; next non-empty would *narrow* → reject.
    // - prev non-empty → next must be a superset (or empty, which means open).
    if prev.host_allow.is_empty() && !next.host_allow.is_empty() {
        return Err(GrantValidationError::NetworkHostAllowNarrowed {
            prev: prev.host_allow.clone(),
            next: next.host_allow.clone(),
        });
    }
    if !prev.host_allow.is_empty() && !next.host_allow.is_empty() {
        let removed: Vec<String> = prev
            .host_allow
            .iter()
            .filter(|h| !next.host_allow.contains(h))
            .cloned()
            .collect();
        if !removed.is_empty() {
            return Err(GrantValidationError::NetworkHostAllowNarrowed {
                prev: prev.host_allow.clone(),
                next: next.host_allow.clone(),
            });
        }
    }

    Ok(())
}

fn validate_exec_widening(
    prev: &ExecPolicy,
    next: &ExecPolicy,
) -> Result<(), GrantValidationError> {
    if prev.enabled && !next.enabled {
        return Err(GrantValidationError::ExecDisabledByGrant);
    }

    // always_deny_substrings is invariant under grants — adding entries
    // narrows what can be executed (a previously-allowed command containing
    // the new substring flips Allow → Deny); removing entries weakens
    // built-in safety guards. Modifications belong in envelope construction.
    if prev.always_deny_substrings != next.always_deny_substrings {
        return Err(GrantValidationError::ExecAlwaysDenyChanged {
            prev: prev.always_deny_substrings.clone(),
            next: next.always_deny_substrings.clone(),
        });
    }

    Ok(())
}

/// `next` widens `prev` if it asks for *less* approval (or the same).
/// Order: `PerMajorStep` (strict) < `None` (lax). Going strict→lax widens.
fn cadence_widens(prev: ApprovalCadence, next: ApprovalCadence) -> bool {
    match (prev, next) {
        (ApprovalCadence::PerMajorStep, _) => true, // any → wider or same
        (ApprovalCadence::None, ApprovalCadence::None) => true,
        (ApprovalCadence::None, ApprovalCadence::PerMajorStep) => false, // tightening
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{
        ApprovalCadence, EnvelopeScope, ExecPolicy, FanoutPolicy, NetworkPolicy, PathAllowlist,
        PermissionEnvelope,
    };

    fn base() -> PermissionEnvelope {
        PermissionEnvelope::act_preset(vec!["src/**".into()], vec![])
    }

    // ── Identity / idempotency ─────────────────────────────────────────────

    #[test]
    fn identity_grant_is_idempotent_and_passes() {
        let env = base();
        let mutator = DefaultEnvelopeMutator;
        let out = mutator.apply_grant(&env, &env).expect("identity ok");
        assert_eq!(out, env);

        // applying twice yields same result (idempotency)
        let out2 = mutator.apply_grant(&out, &env).expect("idempotent");
        assert_eq!(out2, env);
    }

    // ── Scope invariance ───────────────────────────────────────────────────

    #[test]
    fn scope_change_rejected() {
        let prev = base();
        let mut next = prev.clone();
        next.scope = EnvelopeScope::Session;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(err, GrantValidationError::ScopeChanged { .. }));
    }

    // ── Untrusted-output invariance ────────────────────────────────────────

    #[test]
    fn relaxing_untrusted_output_rejected() {
        let prev = base();
        let mut next = prev.clone();
        next.treat_tool_output_as_untrusted = false;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::UntrustedOutputFlagChanged { .. }
        ));
    }

    #[test]
    fn tightening_untrusted_output_also_rejected() {
        // Even tightening is rejected — the flag is invariant across grants.
        let mut prev = base();
        prev.treat_tool_output_as_untrusted = false;
        let mut next = prev.clone();
        next.treat_tool_output_as_untrusted = true;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::UntrustedOutputFlagChanged { .. }
        ));
    }

    // ── Fanout invariance ──────────────────────────────────────────────────

    #[test]
    fn fanout_policy_change_rejected() {
        let prev = base();
        let mut next = prev.clone();
        next.fanout_policy = FanoutPolicy::Off;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::FanoutPolicyChanged { .. }
        ));
    }

    // ── Path allowlists ────────────────────────────────────────────────────

    #[test]
    fn write_allow_growing_accepted() {
        let prev = base(); // src/**
        let mut next = prev.clone();
        next.write.allow.push("docs/**".into());
        DefaultEnvelopeMutator
            .apply_grant(&prev, &next)
            .expect("growing write.allow widens");
    }

    #[test]
    fn write_allow_shrinking_rejected() {
        let prev = base();
        let mut next = prev.clone();
        next.write.allow.clear(); // removed src/**
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(err, GrantValidationError::WriteAllowShrunk { .. }));
    }

    #[test]
    fn write_deny_must_be_invariant() {
        // Adding an entry to deny narrows what was previously allowed.
        let mut prev = base();
        prev.write.deny.push(".env".into());
        let mut next = prev.clone();
        next.write.deny.clear();
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(err, GrantValidationError::WriteDenyChanged { .. }));
    }

    /// Regression for rubber-duck finding 1: deny-list growth was previously
    /// accepted as "append-only widening" but actually narrows — a path
    /// matched by both `allow` and the new `deny` flips Allow → Deny.
    #[test]
    fn write_deny_growth_revoking_path_rejected() {
        let prev = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]);
        let mut next = prev.clone();
        next.write.deny.push("src/**".into());
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(
            matches!(err, GrantValidationError::WriteDenyChanged { .. }),
            "deny-growth must be rejected — it revokes previously-allowed writes"
        );
    }

    #[test]
    fn read_intercept_relax_rejected() {
        let mut prev = base();
        prev.read = PathAllowlist {
            allow: vec!["src/**".into()],
            deny: vec![],
            intercept_denied: true,
        };
        let mut next = prev.clone();
        next.read.intercept_denied = false;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert_eq!(err, GrantValidationError::ReadInterceptRelaxed);
    }

    // ── Network ────────────────────────────────────────────────────────────

    #[test]
    fn network_disable_by_grant_rejected() {
        let prev = base(); // network open
        let mut next = prev.clone();
        next.network = NetworkPolicy::disabled();
        let err = validate_widening(&prev, &next).unwrap_err();
        assert_eq!(err, GrantValidationError::NetworkDisabledByGrant);
    }

    #[test]
    fn network_enable_by_grant_accepted() {
        let mut prev = base();
        prev.network = NetworkPolicy::disabled();
        let mut next = prev.clone();
        next.network = NetworkPolicy::open();
        DefaultEnvelopeMutator
            .apply_grant(&prev, &next)
            .expect("disabled → open widens");
    }

    #[test]
    fn network_host_allow_narrowing_rejected() {
        let mut prev = base();
        prev.network = NetworkPolicy {
            enabled: true,
            host_allow: vec![],
            host_deny: vec![],
        };
        let mut next = prev.clone();
        next.network.host_allow = vec!["github.com".into()]; // narrowing from open → only github
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::NetworkHostAllowNarrowed { .. }
        ));
    }

    #[test]
    fn network_host_allow_growing_accepted() {
        let mut prev = base();
        prev.network = NetworkPolicy {
            enabled: true,
            host_allow: vec!["github.com".into()],
            host_deny: vec![],
        };
        let mut next = prev.clone();
        next.network.host_allow.push("crates.io".into());
        DefaultEnvelopeMutator
            .apply_grant(&prev, &next)
            .expect("growing host_allow widens");
    }

    #[test]
    fn network_host_deny_must_be_invariant() {
        let mut prev = base();
        prev.network = NetworkPolicy {
            enabled: true,
            host_allow: vec![],
            host_deny: vec!["evil.example".into()],
        };
        let mut next = prev.clone();
        next.network.host_deny.clear();
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::NetworkHostDenyChanged { .. }
        ));
    }

    /// Regression for rubber-duck finding 1: deny-list growth on
    /// `host_deny` revokes a previously-allowed host. Must be rejected.
    #[test]
    fn network_host_deny_growth_revoking_host_rejected() {
        let mut prev = base();
        prev.network = NetworkPolicy {
            enabled: true,
            host_allow: vec!["github.com".into()],
            host_deny: vec![],
        };
        let mut next = prev.clone();
        next.network.host_deny.push("github.com".into());
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::NetworkHostDenyChanged { .. }
        ));
    }

    // ── Exec ───────────────────────────────────────────────────────────────

    #[test]
    fn exec_disable_by_grant_rejected() {
        let mut prev = base();
        prev.exec = ExecPolicy::enabled_with_guards();
        let mut next = prev.clone();
        next.exec.enabled = false;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert_eq!(err, GrantValidationError::ExecDisabledByGrant);
    }

    #[test]
    fn exec_enable_by_grant_accepted() {
        let mut prev = base();
        prev.exec = ExecPolicy::disabled();
        let mut next = prev.clone();
        next.exec = ExecPolicy::enabled_with_guards();
        DefaultEnvelopeMutator
            .apply_grant(&prev, &next)
            .expect("disabled → enabled widens");
    }

    #[test]
    fn exec_always_deny_substrings_must_be_invariant() {
        // Built-in guards (rm -rf /, mkfs., etc.) cannot be removed.
        let prev = base(); // exec enabled with default guards
        let mut next = prev.clone();
        next.exec.always_deny_substrings.clear();
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::ExecAlwaysDenyChanged { .. }
        ));
    }

    /// Regression for rubber-duck finding 1: deny-substring growth revokes
    /// a previously-allowed command class. Must be rejected.
    #[test]
    fn exec_always_deny_substrings_growth_revoking_command_rejected() {
        let mut prev = base();
        prev.exec = ExecPolicy::enabled_with_guards();
        let mut next = prev.clone();
        next.exec.always_deny_substrings.push("cargo ".into());
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::ExecAlwaysDenyChanged { .. }
        ));
    }

    // ── Cadence ────────────────────────────────────────────────────────────

    #[test]
    fn cadence_relax_to_none_accepted() {
        let prev = base(); // PerMajorStep
        let mut next = prev.clone();
        next.approval_cadence = ApprovalCadence::None;
        DefaultEnvelopeMutator
            .apply_grant(&prev, &next)
            .expect("PerMajorStep → None widens");
    }

    #[test]
    fn cadence_tighten_rejected() {
        let mut prev = base();
        prev.approval_cadence = ApprovalCadence::None;
        let mut next = prev.clone();
        next.approval_cadence = ApprovalCadence::PerMajorStep;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(err, GrantValidationError::CadenceTightened { .. }));
    }

    // ── Skill budget ───────────────────────────────────────────────────────

    #[test]
    fn skill_budget_growth_accepted() {
        let prev = base();
        let mut next = prev.clone();
        next.skill_budget = prev.skill_budget + 4;
        DefaultEnvelopeMutator
            .apply_grant(&prev, &next)
            .expect("budget growth widens");
    }

    #[test]
    fn skill_budget_shrinkage_rejected() {
        let prev = base();
        let mut next = prev.clone();
        next.skill_budget = prev.skill_budget.saturating_sub(1);
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(
            err,
            GrantValidationError::SkillBudgetShrunk { .. }
        ));
    }

    // ── End-to-end realistic grants ────────────────────────────────────────

    #[test]
    fn grant_adds_one_path_to_write_allow() {
        // The most common real-world grant: agent asks for write access to
        // a specific file, user approves.
        let prev = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]);
        let mut next = prev.clone();
        next.write.allow.push("config/app.toml".into());
        let out = DefaultEnvelopeMutator.apply_grant(&prev, &next).unwrap();
        assert_eq!(out.write.allow, vec!["src/**", "config/app.toml"]);
    }

    #[test]
    fn grant_promoting_act_to_autopilot_accepted() {
        // PerMajorStep → None, plus untrusted-output and fanout must stay
        // identical between act and autopilot presets.
        let prev = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]);
        let next = PermissionEnvelope::autopilot_preset(vec!["src/**".into()], vec![]);
        DefaultEnvelopeMutator
            .apply_grant(&prev, &next)
            .expect("act → autopilot is widening (cadence relaxes, others equal)");
    }

    #[test]
    fn grant_demoting_autopilot_to_act_rejected() {
        let prev = PermissionEnvelope::autopilot_preset(vec!["src/**".into()], vec![]);
        let next = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]);
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(err, GrantValidationError::CadenceTightened { .. }));
    }

    #[test]
    fn grant_validation_short_circuits_on_first_error() {
        // Multiple violations: scope change AND budget shrink. We expect
        // ScopeChanged because invariants run before field-by-field widening.
        let prev = base();
        let mut next = prev.clone();
        next.scope = EnvelopeScope::Session;
        next.skill_budget = 0;
        let err = validate_widening(&prev, &next).unwrap_err();
        assert!(matches!(err, GrantValidationError::ScopeChanged { .. }));
    }

    // ── ST8 PR-3D — propose_widened_envelope ───────────────────────────────

    #[test]
    fn propose_widen_read_appends_to_allow() {
        let env = base(); // act_preset has read open-all (allow=[]). Use a non-empty allow envelope.
        let mut env = env;
        env.read = PathAllowlist {
            allow: vec!["src/**".into()],
            deny: vec![],
            intercept_denied: false,
        };
        let next = propose_widened_envelope(&env, "read", "docs/**").unwrap();
        assert_eq!(next.read.allow, vec!["src/**", "docs/**"]);
        validate_widening(&env, &next).expect("monotonic widening");
    }

    #[test]
    fn propose_widen_write_appends_to_allow() {
        let env = base(); // act_preset write_allow=["src/**"]
        let next = propose_widened_envelope(&env, "write", "tests/**").unwrap();
        assert!(next.write.allow.contains(&"tests/**".to_string()));
        assert!(next.write.allow.contains(&"src/**".to_string()));
        validate_widening(&env, &next).expect("monotonic widening");
    }

    #[test]
    fn propose_widen_open_allowlist_appends() {
        // PathAllowlist::open_all() is `["**"]` — appending another entry
        // is a strict superset and validates fine. (Empty allow means
        // "deny all", not "open all", per PathAllowlist::check semantics.)
        let env = base();
        assert_eq!(env.read.allow, vec!["**".to_string()]);
        let next = propose_widened_envelope(&env, "read", "/etc/passwd").unwrap();
        assert_eq!(next.read.allow, vec!["**", "/etc/passwd"]);
        validate_widening(&env, &next).expect("monotonic widening");
    }

    #[test]
    fn propose_widen_empty_path_allow_grows_to_singleton() {
        // Plan-mode write allow is empty (deny-all). Granting one entry
        // is a legitimate widening.
        let mut env = base();
        env.write.allow = vec![];
        let next = propose_widened_envelope(&env, "write", "src/foo.rs").unwrap();
        assert_eq!(next.write.allow, vec!["src/foo.rs"]);
        validate_widening(&env, &next).expect("empty→singleton is widening");
    }

    #[test]
    fn propose_widen_idempotent_when_already_present() {
        let mut env = base();
        env.write.allow = vec!["src/**".into(), "docs/**".into()];
        let next = propose_widened_envelope(&env, "write", "docs/**").unwrap();
        assert_eq!(next.write.allow, env.write.allow, "no duplicate");
        validate_widening(&env, &next).expect("identity ok");
    }

    #[test]
    fn propose_widen_network_enables_and_appends_host() {
        let mut env = base();
        env.network = NetworkPolicy {
            enabled: false,
            host_allow: vec!["example.com".into()],
            host_deny: vec![],
        };
        let next = propose_widened_envelope(&env, "network", "api.openai.com").unwrap();
        assert!(next.network.enabled);
        assert_eq!(
            next.network.host_allow,
            vec!["example.com", "api.openai.com"]
        );
        validate_widening(&env, &next).expect("monotonic widening");
    }

    #[test]
    fn propose_widen_network_open_host_allow_stays_open() {
        let mut env = base();
        env.network = NetworkPolicy {
            enabled: false,
            host_allow: vec![],
            host_deny: vec![],
        };
        let next = propose_widened_envelope(&env, "network", "api.openai.com").unwrap();
        assert!(next.network.enabled, "must enable");
        assert!(
            next.network.host_allow.is_empty(),
            "open-all host_allow stays open-all"
        );
        validate_widening(&env, &next).expect("monotonic widening");
    }

    #[test]
    fn propose_widen_exec_enables_only() {
        let mut env = base();
        env.exec = ExecPolicy::disabled();
        let prev_deny = env.exec.always_deny_substrings.clone();
        let next = propose_widened_envelope(&env, "exec", "git status").unwrap();
        assert!(next.exec.enabled);
        assert_eq!(
            next.exec.always_deny_substrings, prev_deny,
            "safety guards must be invariant"
        );
        validate_widening(&env, &next).expect("monotonic widening");
    }

    #[test]
    fn propose_widen_unknown_capability_errors() {
        let env = base();
        let err = propose_widened_envelope(&env, "telepathy", "neighbor's mind").unwrap_err();
        assert!(matches!(err, WidenProposalError::UnknownCapability { .. }));
    }

    #[test]
    fn propose_widen_empty_resource_errors() {
        let env = base();
        let err = propose_widened_envelope(&env, "read", "   ").unwrap_err();
        assert!(matches!(err, WidenProposalError::EmptyResource { .. }));
    }

    #[test]
    fn propose_widen_preserves_safety_invariants() {
        // Prove that scope/untrusted-flag/fanout/cadence/skill_budget are all
        // preserved across every capability mapping.
        let mut env = base();
        env.scope = EnvelopeScope::MajorStep;
        env.treat_tool_output_as_untrusted = true;
        env.fanout_policy = FanoutPolicy::MultiPersona;
        env.approval_cadence = ApprovalCadence::PerMajorStep;
        env.skill_budget = 7;
        env.read.intercept_denied = true;
        env.write.intercept_denied = true;
        env.network.host_deny = vec!["evil.com".into()];

        for (cap, res) in [
            ("read", "docs/**"),
            ("write", "tests/**"),
            ("network", "api.example.com"),
            ("exec", "git status"),
        ] {
            let next = propose_widened_envelope(&env, cap, res)
                .unwrap_or_else(|e| panic!("{cap}/{res}: {e}"));
            assert_eq!(next.scope, env.scope, "{cap}: scope preserved");
            assert_eq!(
                next.treat_tool_output_as_untrusted, env.treat_tool_output_as_untrusted,
                "{cap}: untrusted flag preserved"
            );
            assert_eq!(
                next.fanout_policy, env.fanout_policy,
                "{cap}: fanout preserved"
            );
            assert_eq!(
                next.approval_cadence, env.approval_cadence,
                "{cap}: cadence preserved"
            );
            assert_eq!(
                next.skill_budget, env.skill_budget,
                "{cap}: skill_budget preserved"
            );
            assert_eq!(
                next.read.intercept_denied, env.read.intercept_denied,
                "{cap}: read.intercept_denied preserved"
            );
            assert_eq!(
                next.write.intercept_denied, env.write.intercept_denied,
                "{cap}: write.intercept_denied preserved"
            );
            assert_eq!(
                next.network.host_deny, env.network.host_deny,
                "{cap}: host_deny preserved"
            );
            assert_eq!(
                next.exec.always_deny_substrings, env.exec.always_deny_substrings,
                "{cap}: exec deny substrings preserved"
            );
            // The big property: every output round-trips through the validator.
            validate_widening(&env, &next)
                .unwrap_or_else(|e| panic!("{cap}/{res}: validate_widening rejected: {e}"));
        }
    }
}
