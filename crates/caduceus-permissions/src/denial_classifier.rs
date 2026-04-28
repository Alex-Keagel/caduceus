//! Denial classifier — decides whether a permission denial should
//! suggest switching to a more permissive canonical mode (e.g. Plan →
//! Act for a write) or fall through to the existing scope-expansion
//! grant flow.
//!
//! This is the pure-types layer of the smart-profile-routing design
//! (see session-state denial-routing-design.md). PR-B1 ships only the
//! types + `classical_fit` table + `DenialClassification::classify`.
//! PR-B2 wires it into the orchestrator's preflight pipeline.

use crate::envelope::{DenyReason, ExpansionCapability};
use serde::{Deserialize, Serialize};

/// Identity of one of the canonical envelope presets, plus an escape
/// hatch (`Custom`) for user-defined modes. Used by [`classical_fit`]
/// to decide whether a denial has a more-permissive mode that
/// classically owns this kind of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModeKind {
    Plan,
    Research,
    Act,
    Autopilot,
    /// Any mode name not in the canonical set. `classical_fit` always
    /// returns `None` for `Custom` so the user keeps existing behavior.
    Custom,
}

impl ModeKind {
    /// Map a mode name (matching [`crate::PermissionEnvelope::from_mode_name`]'s
    /// accepted set) to a `ModeKind`. Case-insensitive. Unknown names
    /// → [`ModeKind::Custom`].
    ///
    /// Common community aliases collapse to their nearest canonical
    /// kind so a denial classifier never accidentally falls through to
    /// `Custom` (which short-circuits the deny-routing table) just
    /// because someone wired a UI label like "architect" or "debug"
    /// into the orchestrator. Defence-in-depth — today the
    /// orchestrator always emits canonical names, but if that
    /// invariant ever breaks, deny routing keeps working.
    pub fn from_mode_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "plan" | "architect" | "arch" | "planning" => Self::Plan,
            "research" | "explore" | "investigate" => Self::Research,
            "act" | "code" | "coding" | "implement" | "debug" | "dbg" | "review" => Self::Act,
            "autopilot" | "auto" | "yolo" => Self::Autopilot,
            _ => Self::Custom,
        }
    }

    /// Canonical lower-case name for this kind. `Custom` returns
    /// `None` since there is no canonical name for a user-defined mode.
    pub fn canonical_name(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some("plan"),
            Self::Research => Some("research"),
            Self::Act => Some("act"),
            Self::Autopilot => Some("autopilot"),
            Self::Custom => None,
        }
    }
}

/// Classical-fit table: given a denial in `current` mode for `capability`,
/// is there a more permissive canonical mode that classically owns
/// this kind of work? Returns the suggested target mode, or `None` to
/// fall through to the existing scope-expansion grant flow.
///
/// Rules (mirrors design doc):
/// - **Plan / Research + Write or Exec → Act.** Plan is read-only by
///   design; Research allows only `.md` writes — any other write or
///   exec attempt is classical Act work.
/// - **Read denials → None.** Reads are open in every preset; if a
///   read is denied (e.g. by an explicit `read.deny` glob in a custom
///   envelope) the user should grant or refuse explicitly, not switch
///   modes.
/// - **Network denials → None.** Network is gated by host-block lists,
///   not by mode. A network denial is a real grant ask, not a mode
///   mismatch.
/// - **Act / Autopilot → None for everything.** Both already span
///   writes + exec; any denial there is genuinely out-of-scope (out-of-
///   allow path, sensitive path, host-block) and needs an explicit grant.
/// - **Custom → None.** User-defined modes have no canonical fit; users
///   keep the existing grant UX.
///
/// Note: this function does **not** consider the denial's
/// [`DenyReason`]. The sensitive-path bypass lives in
/// [`DenialClassification::classify`], so this table can stay a pure
/// `(mode, capability) → mode` mapping.
pub fn classical_fit(current: ModeKind, capability: ExpansionCapability) -> Option<ModeKind> {
    use ExpansionCapability::*;
    use ModeKind::*;
    match (current, capability) {
        (Plan, Write) | (Plan, Exec) => Some(Act),
        (Research, Write) | (Research, Exec) => Some(Act),
        _ => None,
    }
}

/// Outcome of classifying a permission denial. Built on top of the
/// `(capability, resource, reason)` triple already produced by the
/// preflight pipeline.
///
/// Sensitive-path denials always route to [`DenialClassification::GrantRequired`]
/// regardless of mode. The user always confirms `private/**` writes
/// — switching to a more permissive mode does NOT unlock them, because
/// every preset has the same sensitive-path block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenialClassification {
    /// Action is classical for `target_mode`. UI offers a profile switch.
    SuggestProfileSwitch {
        target_mode: ModeKind,
        capability: ExpansionCapability,
        resource: String,
    },
    /// No classical fit (or sensitive path). Fall through to scope-
    /// expansion grant flow.
    GrantRequired {
        capability: ExpansionCapability,
        resource: String,
        reason: DenyReason,
    },
}

impl DenialClassification {
    /// Classify a denial.
    ///
    /// `current` is the mode the agent is currently running under;
    /// `capability`, `resource`, and `reason` come from the preflight
    /// pipeline's `format_preflight_outcome` triple.
    ///
    /// Sensitive-path bypass: if `reason` is
    /// [`DenyReason::SensitivePath`], the result is always
    /// `GrantRequired`. The classical-fit table is only consulted for
    /// non-sensitive denials.
    pub fn classify(
        current: ModeKind,
        capability: ExpansionCapability,
        resource: String,
        reason: DenyReason,
    ) -> Self {
        if matches!(reason, DenyReason::SensitivePath(_)) {
            return Self::GrantRequired {
                capability,
                resource,
                reason,
            };
        }
        match classical_fit(current, capability.clone()) {
            Some(target) => Self::SuggestProfileSwitch {
                target_mode: target,
                capability,
                resource,
            },
            None => Self::GrantRequired {
                capability,
                resource,
                reason,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ModeKind ────────────────────────────────────────────────────

    #[test]
    fn mode_kind_from_canonical_names() {
        assert_eq!(ModeKind::from_mode_name("plan"), ModeKind::Plan);
        assert_eq!(ModeKind::from_mode_name("research"), ModeKind::Research);
        assert_eq!(ModeKind::from_mode_name("act"), ModeKind::Act);
        assert_eq!(ModeKind::from_mode_name("autopilot"), ModeKind::Autopilot);
    }

    #[test]
    fn mode_kind_is_case_insensitive() {
        assert_eq!(ModeKind::from_mode_name("PLAN"), ModeKind::Plan);
        assert_eq!(ModeKind::from_mode_name("Act"), ModeKind::Act);
        assert_eq!(ModeKind::from_mode_name("AutoPilot"), ModeKind::Autopilot);
    }

    #[test]
    fn unknown_mode_names_become_custom() {
        // Truly unrecognised names — not in the canonical set, not in
        // the alias set — collapse to Custom.
        assert_eq!(ModeKind::from_mode_name(""), ModeKind::Custom);
        assert_eq!(ModeKind::from_mode_name("plan-strict"), ModeKind::Custom);
        assert_eq!(ModeKind::from_mode_name("xyzzy"), ModeKind::Custom);
    }

    /// Common community aliases must collapse to their nearest
    /// canonical kind so deny-routing keeps working even if the
    /// orchestrator forwards a non-canonical name. Today this is
    /// theoretical (orchestrator always emits canonical names) — this
    /// test pins the contract so the alias defence doesn't silently
    /// regress.
    #[test]
    fn common_mode_aliases_collapse_to_canonical_kinds() {
        for (alias, kind) in [
            ("architect", ModeKind::Plan),
            ("ARCH", ModeKind::Plan),
            ("planning", ModeKind::Plan),
            ("explore", ModeKind::Research),
            ("Investigate", ModeKind::Research),
            ("code", ModeKind::Act),
            ("CODING", ModeKind::Act),
            ("implement", ModeKind::Act),
            ("debug", ModeKind::Act),
            ("dbg", ModeKind::Act),
            ("review", ModeKind::Act),
            ("auto", ModeKind::Autopilot),
            ("YOLO", ModeKind::Autopilot),
        ] {
            assert_eq!(
                ModeKind::from_mode_name(alias),
                kind,
                "alias {alias:?} should collapse to {kind:?}"
            );
        }
    }

    #[test]
    fn canonical_name_round_trips_for_canonical_modes() {
        for kind in [
            ModeKind::Plan,
            ModeKind::Research,
            ModeKind::Act,
            ModeKind::Autopilot,
        ] {
            let name = kind.canonical_name().expect("canonical name");
            assert_eq!(ModeKind::from_mode_name(name), kind);
        }
    }

    #[test]
    fn custom_has_no_canonical_name() {
        assert_eq!(ModeKind::Custom.canonical_name(), None);
    }

    // ── classical_fit table ─────────────────────────────────────────

    #[test]
    fn plan_writes_and_exec_route_to_act() {
        assert_eq!(
            classical_fit(ModeKind::Plan, ExpansionCapability::Write),
            Some(ModeKind::Act)
        );
        assert_eq!(
            classical_fit(ModeKind::Plan, ExpansionCapability::Exec),
            Some(ModeKind::Act)
        );
    }

    #[test]
    fn research_writes_and_exec_route_to_act() {
        assert_eq!(
            classical_fit(ModeKind::Research, ExpansionCapability::Write),
            Some(ModeKind::Act)
        );
        assert_eq!(
            classical_fit(ModeKind::Research, ExpansionCapability::Exec),
            Some(ModeKind::Act)
        );
    }

    #[test]
    fn read_denials_never_suggest_a_switch() {
        for mode in [
            ModeKind::Plan,
            ModeKind::Research,
            ModeKind::Act,
            ModeKind::Autopilot,
            ModeKind::Custom,
        ] {
            assert_eq!(
                classical_fit(mode, ExpansionCapability::Read),
                None,
                "mode={:?}",
                mode
            );
        }
    }

    #[test]
    fn network_denials_never_suggest_a_switch() {
        // Network is host-list gated, not mode gated. A network denial
        // is always a real grant ask — switching modes wouldn't change
        // the host-block list.
        for mode in [
            ModeKind::Plan,
            ModeKind::Research,
            ModeKind::Act,
            ModeKind::Autopilot,
            ModeKind::Custom,
        ] {
            assert_eq!(
                classical_fit(mode, ExpansionCapability::Network),
                None,
                "mode={:?}",
                mode
            );
        }
    }

    #[test]
    fn act_and_autopilot_have_no_classical_fit() {
        for cap in [
            ExpansionCapability::Read,
            ExpansionCapability::Write,
            ExpansionCapability::Network,
            ExpansionCapability::Exec,
        ] {
            assert_eq!(
                classical_fit(ModeKind::Act, cap.clone()),
                None,
                "Act + {:?}",
                cap
            );
            assert_eq!(
                classical_fit(ModeKind::Autopilot, cap.clone()),
                None,
                "Autopilot + {:?}",
                cap
            );
        }
    }

    #[test]
    fn custom_mode_never_has_a_classical_fit() {
        // Users with custom envelopes keep the existing grant UX —
        // we don't second-guess unknown semantics.
        for cap in [
            ExpansionCapability::Read,
            ExpansionCapability::Write,
            ExpansionCapability::Network,
            ExpansionCapability::Exec,
        ] {
            assert_eq!(classical_fit(ModeKind::Custom, cap), None);
        }
    }

    // ── DenialClassification::classify ──────────────────────────────

    #[test]
    fn classify_plan_write_suggests_switch_to_act() {
        let result = DenialClassification::classify(
            ModeKind::Plan,
            ExpansionCapability::Write,
            "src/foo.rs".into(),
            DenyReason::NotInAllowList,
        );
        assert_eq!(
            result,
            DenialClassification::SuggestProfileSwitch {
                target_mode: ModeKind::Act,
                capability: ExpansionCapability::Write,
                resource: "src/foo.rs".into(),
            }
        );
    }

    #[test]
    fn classify_act_write_requires_grant() {
        let result = DenialClassification::classify(
            ModeKind::Act,
            ExpansionCapability::Write,
            "out_of_allow.txt".into(),
            DenyReason::NotInAllowList,
        );
        assert_eq!(
            result,
            DenialClassification::GrantRequired {
                capability: ExpansionCapability::Write,
                resource: "out_of_allow.txt".into(),
                reason: DenyReason::NotInAllowList,
            }
        );
    }

    #[test]
    fn sensitive_path_in_act_requires_grant() {
        let result = DenialClassification::classify(
            ModeKind::Act,
            ExpansionCapability::Write,
            "private/audits/foo.md".into(),
            DenyReason::SensitivePath("private/audits/foo.md".into()),
        );
        assert!(matches!(
            result,
            DenialClassification::GrantRequired {
                reason: DenyReason::SensitivePath(_),
                ..
            }
        ));
    }

    #[test]
    fn sensitive_path_overrides_classical_fit_in_plan() {
        // Critical invariant: Plan + Write would normally suggest Act,
        // but sensitive paths must ALWAYS route to grant — switching
        // to Act would not unlock private/** anyway (every preset has
        // the same sensitive block) and we want the user to confirm
        // the path explicitly, not the mode.
        let result = DenialClassification::classify(
            ModeKind::Plan,
            ExpansionCapability::Write,
            "private/notes.md".into(),
            DenyReason::SensitivePath("private/notes.md".into()),
        );
        assert!(
            matches!(result, DenialClassification::GrantRequired { .. }),
            "sensitive path must bypass classical_fit, got {:?}",
            result
        );
    }

    #[test]
    fn classify_custom_mode_falls_through_to_grant() {
        let result = DenialClassification::classify(
            ModeKind::Custom,
            ExpansionCapability::Write,
            "src/foo.rs".into(),
            DenyReason::NotInAllowList,
        );
        assert!(matches!(result, DenialClassification::GrantRequired { .. }));
    }

    #[test]
    fn classify_research_exec_suggests_switch_to_act() {
        let result = DenialClassification::classify(
            ModeKind::Research,
            ExpansionCapability::Exec,
            "cargo build".into(),
            DenyReason::ExecDisabled,
        );
        assert_eq!(
            result,
            DenialClassification::SuggestProfileSwitch {
                target_mode: ModeKind::Act,
                capability: ExpansionCapability::Exec,
                resource: "cargo build".into(),
            }
        );
    }

    #[test]
    fn classify_network_denial_never_suggests_switch() {
        // Network denials always go to the grant flow regardless of
        // mode — the host-block list is mode-independent.
        let result = DenialClassification::classify(
            ModeKind::Plan,
            ExpansionCapability::Network,
            "https://evil.example".into(),
            DenyReason::HostDenied("evil.example".into()),
        );
        assert!(matches!(result, DenialClassification::GrantRequired { .. }));
    }
}
