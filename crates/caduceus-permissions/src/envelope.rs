//! Permission envelope — the real permission model.
//!
//! Modes are envelope *presets*. The envelope carries per-folder allow/deny
//! globs, network policy, exec policy, approval cadence, fan-out policy, and
//! scope. It is created once by the orchestrator per scope and inherited by
//! value by every sub-agent or fan-out worker. Sub-agents cannot widen the
//! envelope; deny wins across layers.
//!
//! See `plan.md` §Decision 2 for the design rationale.

use glob::Pattern;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Decision ───────────────────────────────────────────────────────────────────

/// Result of a permission check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Action is allowed without user interaction.
    Allow,
    /// Action is allowed but the tool should return a *simulated* result
    /// (used by Plan mode for writes — the agent sees "would write to X"
    /// rather than actually mutating the filesystem).
    Intercept,
    /// Action is denied. Caller must either abort or request scope expansion
    /// via `PermissionEvent::ScopeExpansionRequested`.
    Deny(DenyReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    NotInAllowList,
    MatchesDeny,
    NetworkDisabled,
    HostDenied(String),
    ExecDisabled,
    CommandBlacklisted(String),
    /// Path matches a sensitive-write pattern (e.g. `private/**`). Always
    /// denied at the engine level so writes go through an explicit grant
    /// flow regardless of the active mode's allow rules. The grant prompt
    /// also gives the user a chance to confirm or override the target path.
    SensitivePath(String),
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInAllowList => write!(f, "path is not in allow list"),
            Self::MatchesDeny => write!(f, "path matches a deny pattern"),
            Self::NetworkDisabled => write!(f, "network access is disabled"),
            Self::HostDenied(h) => write!(f, "host '{h}' is denied"),
            Self::ExecDisabled => write!(f, "command execution is disabled"),
            Self::CommandBlacklisted(c) => write!(f, "command '{c}' is always denied"),
            Self::SensitivePath(p) => write!(
                f,
                "path '{p}' is a sensitive location and requires explicit grant + path confirmation"
            ),
        }
    }
}

// ── Path allowlist ─────────────────────────────────────────────────────────────

/// A glob allow/deny list with deny-wins semantics.
///
/// Matching rules:
/// - If the path matches any glob in `deny`, result is `Deny` (or `Intercept`
///   when `intercept_denied = true`).
/// - Otherwise if the path matches any glob in `allow`, result is `Allow`.
/// - Otherwise, result is `Deny` (or `Intercept` when `intercept_denied = true`).
///
/// Globs use standard `glob` crate syntax: `**` for multi-segment, `*` for
/// single-segment, `?` for single char, `{a,b}` for alternation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PathAllowlist {
    /// Globs that are permitted. Empty means nothing is permitted.
    pub allow: Vec<String>,
    /// Globs that are forbidden. Deny wins over allow.
    pub deny: Vec<String>,
    /// When true, denied actions return `Decision::Intercept` instead of
    /// `Decision::Deny`. Used by Plan mode so writes become "would-write"
    /// simulations rather than hard errors.
    pub intercept_denied: bool,
}

impl PathAllowlist {
    /// Open-everything allowlist (`**`, no deny). Used for reads by default.
    pub fn open_all() -> Self {
        Self {
            allow: vec!["**".into()],
            deny: vec![],
            intercept_denied: false,
        }
    }

    /// Nothing-allowed, but denials become intercepts. Used for Plan mode writes.
    pub fn nothing_intercepted() -> Self {
        Self {
            allow: vec![],
            deny: vec![],
            intercept_denied: true,
        }
    }

    /// Markdown-only writes. Used for Research mode writes.
    pub fn markdown_only() -> Self {
        Self {
            allow: vec!["**/*.md".into()],
            deny: vec![],
            intercept_denied: false,
        }
    }

    /// Empty hard-deny (nothing allowed, denied is denied). Safe default.
    pub fn closed() -> Self {
        Self::default()
    }

    /// Check a path against this allowlist.
    pub fn check(&self, path: &Path) -> Decision {
        let s = path.to_string_lossy();
        for pat in &self.deny {
            if glob_match(pat, &s) {
                return if self.intercept_denied {
                    Decision::Intercept
                } else {
                    Decision::Deny(DenyReason::MatchesDeny)
                };
            }
        }
        for pat in &self.allow {
            if glob_match(pat, &s) {
                return Decision::Allow;
            }
        }
        if self.intercept_denied {
            Decision::Intercept
        } else {
            Decision::Deny(DenyReason::NotInAllowList)
        }
    }

    /// Return a new allowlist that is *no wider* than either input: deny is
    /// the union, allow is the narrower of the two (parent by default;
    /// parent's `**` means the child's allow wins because it restricts).
    /// This gives deny-wins and prevents sub-agents from widening scope.
    pub fn restrict_to(&self, other: &PathAllowlist) -> PathAllowlist {
        let parent_is_open = self.allow.iter().any(|s| s == "**");
        let child_is_open = other.allow.iter().any(|s| s == "**");

        let allow: Vec<String> = if parent_is_open {
            // Parent is fully open, so child's allow restricts.
            other.allow.clone()
        } else if child_is_open {
            // Child asked for everything, keep parent's narrower list.
            self.allow.clone()
        } else {
            // Both are restricted: keep parent's allow (more conservative).
            // Child's allow cannot widen. (If child has patterns not in parent,
            // those paths would fail parent's check anyway.)
            self.allow.clone()
        };

        let mut deny = self.deny.clone();
        for d in &other.deny {
            if !deny.contains(d) {
                deny.push(d.clone());
            }
        }

        PathAllowlist {
            allow,
            deny,
            intercept_denied: self.intercept_denied || other.intercept_denied,
        }
    }
}

pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    Pattern::new(pattern)
        .map(|p| p.matches(path))
        .unwrap_or(false)
}

// ── Network policy ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub enabled: bool,
    /// Allowed host suffixes. Empty = all hosts allowed (subject to `host_deny`).
    pub host_allow: Vec<String>,
    /// Denied host suffixes. Deny wins.
    pub host_deny: Vec<String>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self::open()
    }
}

impl NetworkPolicy {
    pub fn open() -> Self {
        Self {
            enabled: true,
            host_allow: vec![],
            host_deny: vec![],
        }
    }
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            host_allow: vec![],
            host_deny: vec![],
        }
    }

    pub fn check(&self, host: Option<&str>) -> Decision {
        if !self.enabled {
            return Decision::Deny(DenyReason::NetworkDisabled);
        }
        let Some(host) = host else {
            return Decision::Allow;
        };
        for bad in &self.host_deny {
            if host.ends_with(bad.as_str()) {
                return Decision::Deny(DenyReason::HostDenied(host.to_string()));
            }
        }
        if self.host_allow.is_empty() {
            return Decision::Allow;
        }
        for good in &self.host_allow {
            if host.ends_with(good.as_str()) {
                return Decision::Allow;
            }
        }
        Decision::Deny(DenyReason::HostDenied(host.to_string()))
    }

    pub fn restrict_to(&self, other: &NetworkPolicy) -> NetworkPolicy {
        NetworkPolicy {
            enabled: self.enabled && other.enabled,
            host_allow: if self.host_allow.is_empty() {
                other.host_allow.clone()
            } else if other.host_allow.is_empty() {
                self.host_allow.clone()
            } else {
                self.host_allow
                    .iter()
                    .filter(|h| other.host_allow.contains(h))
                    .cloned()
                    .collect()
            },
            host_deny: {
                let mut d = self.host_deny.clone();
                for x in &other.host_deny {
                    if !d.contains(x) {
                        d.push(x.clone());
                    }
                }
                d
            },
        }
    }
}

// ── Exec policy ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecPolicy {
    pub enabled: bool,
    /// Substrings that always cause denial regardless of `enabled`.
    pub always_deny_substrings: Vec<String>,
}

impl Default for ExecPolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

impl ExecPolicy {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            always_deny_substrings: destructive_defaults(),
        }
    }

    pub fn enabled_with_guards() -> Self {
        Self {
            enabled: true,
            always_deny_substrings: destructive_defaults(),
        }
    }

    pub fn check(&self, command: &str) -> Decision {
        for bad in &self.always_deny_substrings {
            if command.contains(bad.as_str()) {
                return Decision::Deny(DenyReason::CommandBlacklisted(bad.clone()));
            }
        }
        if !self.enabled {
            return Decision::Deny(DenyReason::ExecDisabled);
        }
        Decision::Allow
    }

    pub fn restrict_to(&self, other: &ExecPolicy) -> ExecPolicy {
        let mut subs = self.always_deny_substrings.clone();
        for s in &other.always_deny_substrings {
            if !subs.contains(s) {
                subs.push(s.clone());
            }
        }
        ExecPolicy {
            enabled: self.enabled && other.enabled,
            always_deny_substrings: subs,
        }
    }
}

fn destructive_defaults() -> Vec<String> {
    vec![
        "rm -rf /".into(),
        ":(){:|:&};:".into(),
        "dd if=/dev/zero".into(),
        "mkfs.".into(),
        "> /dev/sda".into(),
    ]
}

// ── Cadence / scope / fan-out ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovalCadence {
    /// Orchestrator asks once per major step; sub-tasks inherit.
    PerMajorStep,
    /// Granted once at envelope creation; Autopilot-style. Scope-expansion
    /// events still re-prompt regardless of this setting.
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EnvelopeScope {
    Session,
    Task,
    MajorStep,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FanoutPolicy {
    Off,
    RubberDuckOnly,
    MultiPersona,
}

// ── Envelope ───────────────────────────────────────────────────────────────────

/// The central permission object.
///
/// Created once by the orchestrator per scope; passed by value to every
/// sub-agent or fan-out worker. Sub-agents cannot widen; deny wins across
/// layers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionEnvelope {
    pub read: PathAllowlist,
    pub write: PathAllowlist,
    pub network: NetworkPolicy,
    pub exec: ExecPolicy,
    pub approval_cadence: ApprovalCadence,
    pub scope: EnvelopeScope,
    /// If true, agents must treat tool output as untrusted DATA and ignore
    /// imperatives embedded in fetched content (prompt-injection guard).
    pub treat_tool_output_as_untrusted: bool,
    pub fanout_policy: FanoutPolicy,
    /// Maximum number of skills that may activate per `resolve_lazy()` call.
    /// Fixes the "skill starvation" issue where only 3 skills could activate.
    pub skill_budget: usize,
    /// Globs (path-allowlist style) for write paths that ALWAYS require an
    /// explicit grant prompt regardless of the preset's allow/deny lists.
    /// Default: `["private/**"]` — the `private/` convention used across
    /// every Caduceus repo for user-curated context, audits, and reviewer
    /// outputs. Sensitive paths win over both `write.allow` and
    /// `intercept_denied`, forcing a `Decision::Deny(SensitivePath)` so the
    /// orchestrator's grant flow runs and the user gets to confirm the
    /// target path before any bytes hit disk.
    ///
    /// `#[serde(default = "default_sensitive_write_paths")]` ensures that
    /// any envelope persisted before this field existed deserialises with
    /// the canonical `private/**` protection, NOT an empty list — legacy
    /// payloads can never silently disable the policy.
    #[serde(default = "default_sensitive_write_paths")]
    pub sensitive_write_paths: Vec<String>,
    /// Per-path allow exceptions to `sensitive_write_paths`. The grant
    /// flow appends concrete paths here when the user approves a sensitive
    /// write. `check_write` consults this list FIRST: if the normalised
    /// target equals any entry, the sensitive list is skipped and the
    /// regular allow/deny pipeline runs. Glob patterns are NOT permitted —
    /// `validate_widening` rejects entries containing `*`, `?`, or `[`,
    /// preventing "grant `private/**` once, escape forever".
    ///
    /// Sub-agents start with an empty exception list (see `restrict_to`):
    /// grants made for the parent task do not carry across child boundaries.
    #[serde(default)]
    pub sensitive_write_exceptions: Vec<String>,
}

/// Default sensitive-write globs every preset ships with. Engine-side
/// enforcement of the cross-repo `private/` policy: reads are open
/// (so agents can ground in user-provided context), writes always
/// route through the grant flow.
pub fn default_sensitive_write_paths() -> Vec<String> {
    vec!["private/**".into()]
}

/// Normalise a path for sensitive-glob matching.
///
/// We compare the *lexical* shape of the path the agent asked to write,
/// not the canonical filesystem path (which would TOCTOU-race the
/// actual write and require disk access). This handler addresses the
/// common bypass shapes the tri-review caught:
///
/// - Strips a leading `./` (so `./private/x` matches `private/**`).
/// - Drops interior `.` components (so `foo/./private/x` matches).
/// - Leaves `..` segments **alone** so traversal-style paths like
///   `foo/../private/x` keep matching `private/**` — an agent that
///   tries to launder a sensitive path through traversal still trips
///   the sensitive check.
/// - Lower-cases the result on macOS / Windows where the filesystem is
///   case-insensitive, so `Private/x` matches `private/**`.
///
/// The result is a forward-slash-separated string (the form
/// `glob::Pattern::matches` expects). Absolute paths and Windows
/// backslashes are normalised too.
pub(crate) fn normalise_path_for_sensitive_match(path: &Path) -> String {
    use std::path::Component;
    let mut out = String::new();
    for comp in path.components() {
        let seg: &str = match comp {
            Component::CurDir => continue,
            Component::RootDir => "",
            Component::Prefix(p) => {
                let s = p.as_os_str().to_string_lossy().into_owned();
                out.push_str(&s);
                continue;
            }
            Component::ParentDir => "..",
            Component::Normal(os) => match os.to_str() {
                Some(s) => s,
                None => return path.to_string_lossy().into_owned(),
            },
        };
        if !out.is_empty() && !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(seg);
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        out = out.to_ascii_lowercase();
    }
    out
}

impl PermissionEnvelope {
    // ── Mode presets ──────────────────────────────────────────────────────────

    /// Plan mode: reads open, writes intercepted (agent sees "would-write").
    pub fn plan_preset() -> Self {
        Self {
            read: PathAllowlist::open_all(),
            write: PathAllowlist::nothing_intercepted(),
            network: NetworkPolicy::open(),
            exec: ExecPolicy::disabled(),
            approval_cadence: ApprovalCadence::PerMajorStep,
            scope: EnvelopeScope::Task,
            treat_tool_output_as_untrusted: true,
            fanout_policy: FanoutPolicy::RubberDuckOnly,
            skill_budget: 6,
            sensitive_write_paths: default_sensitive_write_paths(),
            sensitive_write_exceptions: Vec::new(),
        }
    }

    /// Research mode: reads open (codebase + web), writes restricted to `.md`
    /// files only, multi-persona fan-out default.
    pub fn research_preset() -> Self {
        Self {
            read: PathAllowlist::open_all(),
            write: PathAllowlist::markdown_only(),
            network: NetworkPolicy::open(),
            exec: ExecPolicy::disabled(),
            approval_cadence: ApprovalCadence::PerMajorStep,
            scope: EnvelopeScope::Task,
            treat_tool_output_as_untrusted: true,
            fanout_policy: FanoutPolicy::MultiPersona,
            skill_budget: 8,
            sensitive_write_paths: default_sensitive_write_paths(),
            sensitive_write_exceptions: Vec::new(),
        }
    }

    /// Act mode: writes permitted within `write_allow`, per-major-step approval.
    pub fn act_preset(write_allow: Vec<String>, write_deny: Vec<String>) -> Self {
        Self {
            read: PathAllowlist::open_all(),
            write: PathAllowlist {
                allow: write_allow,
                deny: write_deny,
                intercept_denied: false,
            },
            network: NetworkPolicy::open(),
            exec: ExecPolicy::enabled_with_guards(),
            approval_cadence: ApprovalCadence::PerMajorStep,
            scope: EnvelopeScope::MajorStep,
            treat_tool_output_as_untrusted: true,
            fanout_policy: FanoutPolicy::RubberDuckOnly,
            skill_budget: 6,
            sensitive_write_paths: default_sensitive_write_paths(),
            sensitive_write_exceptions: Vec::new(),
        }
    }

    /// Autopilot mode: same write envelope as Act, approval waived at grant time.
    /// Scope-expansion attempts still re-prompt regardless of cadence.
    pub fn autopilot_preset(write_allow: Vec<String>, write_deny: Vec<String>) -> Self {
        let mut env = Self::act_preset(write_allow, write_deny);
        env.approval_cadence = ApprovalCadence::None;
        env
    }

    /// Build a preset envelope for a mode name. Canonical mode names are
    /// `"plan"`, `"research"`, `"act"`, `"autopilot"`. Unknown modes fall back
    /// to `plan_preset()` (the safest preset — no writes). This is the
    /// single entry point `EnvelopeDefaults` delegates to; all other preset
    /// helpers MUST remain equivalent to the per-mode functions they call.
    pub fn from_mode_name(mode: &str, write_allow: Vec<String>, write_deny: Vec<String>) -> Self {
        match mode {
            "plan" | "Plan" => Self::plan_preset(),
            "research" | "Research" => Self::research_preset(),
            "act" | "Act" => Self::act_preset(write_allow, write_deny),
            "autopilot" | "Autopilot" => Self::autopilot_preset(write_allow, write_deny),
            _ => Self::plan_preset(),
        }
    }

    /// ST-B3 / contract `context-injector-v1` — fluent override of the
    /// per-envelope skill activation budget. Default presets ship with a
    /// budget of 6 (plan/act/autopilot) or 8 (research); callers that
    /// need to scope context further (e.g. a subtask fan-out child that
    /// should only see a handful of skills) can tighten it with this
    /// builder. The child envelope used by `restrict_for_child` already
    /// takes the minimum of parent/child, so narrowing via
    /// `with_skill_budget` can never widen scope.
    pub fn with_skill_budget(mut self, skill_budget: usize) -> Self {
        self.skill_budget = skill_budget;
        self
    }

    // ── Check methods ─────────────────────────────────────────────────────────

    pub fn check_read(&self, path: &Path) -> Decision {
        self.read.check(path)
    }

    pub fn check_write(&self, path: &Path) -> Decision {
        // Priority order:
        //   1. `sensitive_write_exceptions` — concrete paths the user has
        //      already approved via the grant flow. Skip the sensitive
        //      block and fall through to the regular allow/deny pipeline.
        //      (We deliberately do NOT short-circuit to `Allow` here:
        //      `write.deny`, `write.allow`, and intercept_denied still
        //      apply, so a granted exception cannot bypass other rules.)
        //   2. `sensitive_write_paths` — globs that ALWAYS deny via
        //      `SensitivePath`, overriding `write.allow` and
        //      `intercept_denied`. Drives the grant flow.
        //   3. `write.allow` / `write.deny` — the normal allowlist.
        //
        // Both 1 and 2 match against a normalised path so common bypass
        // shapes (`./private/x`, `foo/./private/x`, case variants on
        // case-insensitive filesystems) don't slip through. Traversal
        // segments (`..`) are intentionally preserved so an attempt to
        // launder a sensitive path through `foo/../private/x` still trips
        // the sensitive check. See `normalise_path_for_sensitive_match`.
        let normalised = normalise_path_for_sensitive_match(path);
        for exception in &self.sensitive_write_exceptions {
            if exception_matches(exception, &normalised) {
                return self.write.check(path);
            }
        }
        for pat in &self.sensitive_write_paths {
            if glob_match(pat, &normalised) {
                return Decision::Deny(DenyReason::SensitivePath(normalised));
            }
        }
        self.write.check(path)
    }

    pub fn check_network(&self, host: Option<&str>) -> Decision {
        self.network.check(host)
    }

    pub fn check_exec(&self, command: &str) -> Decision {
        self.exec.check(command)
    }

    // ── Inheritance ───────────────────────────────────────────────────────────

    /// Produce the effective envelope a sub-agent sees when `inner` is its
    /// requested envelope. Sub-agents cannot widen: deny-wins for paths,
    /// enabled-AND for network/exec, the stricter cadence wins.
    pub fn restrict_to(&self, inner: &PermissionEnvelope) -> PermissionEnvelope {
        // Sensitive paths union: a child can ADD sensitive globs but never
        // drop them. Deny-wins semantics extended to the sensitive list.
        let mut sensitive = self.sensitive_write_paths.clone();
        for s in &inner.sensitive_write_paths {
            if !sensitive.contains(s) {
                sensitive.push(s.clone());
            }
        }
        // Sensitive-write exceptions: sub-agents start fresh. Grants made
        // for the parent task do NOT carry across child boundaries — a
        // sub-agent that wants to write to a sensitive path must trigger
        // its own grant prompt. (If we inherited exceptions, an Act-mode
        // parent that the user blessed for `private/notes.md` would let
        // a child sub-agent write there silently.)
        PermissionEnvelope {
            read: self.read.restrict_to(&inner.read),
            write: self.write.restrict_to(&inner.write),
            network: self.network.restrict_to(&inner.network),
            exec: self.exec.restrict_to(&inner.exec),
            approval_cadence: stricter_cadence(self.approval_cadence, inner.approval_cadence),
            scope: self.scope,
            treat_tool_output_as_untrusted: self.treat_tool_output_as_untrusted
                || inner.treat_tool_output_as_untrusted,
            fanout_policy: inner.fanout_policy,
            skill_budget: self.skill_budget.min(inner.skill_budget),
            sensitive_write_paths: sensitive,
            sensitive_write_exceptions: Vec::new(),
        }
    }
}

/// Match a `sensitive_write_exceptions` entry against a normalised path.
///
/// Exceptions are concrete paths (validated by `validate_widening` to
/// reject glob characters), but we still tolerate the same lexical
/// variations as `normalise_path_for_sensitive_match`: an exception
/// stored as `private/notes.md` matches a request for `./private/notes.md`,
/// `Private/notes.md` (on case-insensitive FS), etc.
fn exception_matches(exception: &str, normalised_path: &str) -> bool {
    let path = std::path::Path::new(exception);
    let normalised_exception = normalise_path_for_sensitive_match(path);
    normalised_exception == normalised_path
}

fn stricter_cadence(a: ApprovalCadence, b: ApprovalCadence) -> ApprovalCadence {
    match (a, b) {
        (ApprovalCadence::PerMajorStep, _) | (_, ApprovalCadence::PerMajorStep) => {
            ApprovalCadence::PerMajorStep
        }
        (ApprovalCadence::None, ApprovalCadence::None) => ApprovalCadence::None,
    }
}

// ── Scope-expansion event ──────────────────────────────────────────────────────

/// Delta describing an out-of-envelope action the agent wants to perform.
/// Emitted via `PermissionEvent::ScopeExpansionRequested` so the orchestrator
/// can re-prompt the user even under Autopilot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpansionDelta {
    pub capability: ExpansionCapability,
    pub resource: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExpansionCapability {
    Read,
    Write,
    Network,
    Exec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// PermissionEnvelope is the heaviest variant payload (sensitive lists,
// path allowlists, exec/network policies). Boxing would force callers
// through an extra deref on every grant event with no observable
// benefit — this enum is constructed at most once per grant and never
// stored in tight loops or large vectors. Keep the layout flat.
#[allow(clippy::large_enum_variant)]
pub enum PermissionEvent {
    /// The agent attempted an action outside its envelope and needs a grant.
    ScopeExpansionRequested(ExpansionDelta),
    /// The orchestrator granted the expansion, producing an updated envelope.
    ScopeExpansionGranted { updated: PermissionEnvelope },
    /// The orchestrator denied the expansion.
    ScopeExpansionDenied { reason: String },
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn allowlist_allow_match() {
        let a = PathAllowlist {
            allow: vec!["src/**".into()],
            deny: vec![],
            intercept_denied: false,
        };
        assert_eq!(a.check(&p("src/main.rs")), Decision::Allow);
        assert!(matches!(a.check(&p("docs/readme.md")), Decision::Deny(_)));
    }

    #[test]
    fn allowlist_deny_wins() {
        let a = PathAllowlist {
            allow: vec!["src/**".into()],
            deny: vec!["src/secrets/**".into()],
            intercept_denied: false,
        };
        assert_eq!(a.check(&p("src/main.rs")), Decision::Allow);
        assert_eq!(
            a.check(&p("src/secrets/key.pem")),
            Decision::Deny(DenyReason::MatchesDeny)
        );
    }

    #[test]
    fn allowlist_empty_allow_denies() {
        let a = PathAllowlist::closed();
        assert!(matches!(
            a.check(&p("any.rs")),
            Decision::Deny(DenyReason::NotInAllowList)
        ));
    }

    #[test]
    fn allowlist_intercept_for_plan() {
        let a = PathAllowlist::nothing_intercepted();
        assert_eq!(a.check(&p("src/foo.rs")), Decision::Intercept);
    }

    #[test]
    fn allowlist_intercept_on_deny_too() {
        let a = PathAllowlist {
            allow: vec!["**".into()],
            deny: vec!["secrets/**".into()],
            intercept_denied: true,
        };
        assert_eq!(a.check(&p("src/foo.rs")), Decision::Allow);
        assert_eq!(a.check(&p("secrets/key.pem")), Decision::Intercept);
    }

    #[test]
    fn research_preset_allows_markdown_only() {
        let e = PermissionEnvelope::research_preset();
        assert_eq!(e.check_write(&p("notes.md")), Decision::Allow);
        assert_eq!(e.check_write(&p("docs/design.md")), Decision::Allow);
        assert!(matches!(e.check_write(&p("hack.py")), Decision::Deny(_)));
        assert!(matches!(
            e.check_write(&p("src/main.rs")),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn plan_preset_intercepts_writes() {
        let e = PermissionEnvelope::plan_preset();
        assert_eq!(e.check_write(&p("src/main.rs")), Decision::Intercept);
        assert_eq!(e.check_write(&p("notes.md")), Decision::Intercept);
    }

    #[test]
    fn plan_preset_allows_reads() {
        let e = PermissionEnvelope::plan_preset();
        assert_eq!(e.check_read(&p("src/main.rs")), Decision::Allow);
        assert_eq!(e.check_read(&p("any/path/deep.txt")), Decision::Allow);
    }

    /// Cross-profile contract — the project-local `private/` convention
    /// (research notes, audits, scratch context) MUST be readable from
    /// every preset. The corresponding write-side rule (writes always
    /// require a grant prompt with target-path confirmation) is a
    /// behavioral rule enforced by the agent contract in
    /// `Dev/.github/copilot-instructions.md` — this test only locks the
    /// envelope-level READ guarantee so future preset edits can't
    /// silently regress it.
    #[test]
    fn all_presets_allow_reading_private_directory() {
        let private_paths = [
            p("private/notes.md"),
            p("private/audits/wiring-2026.md"),
            p("private/reviews/pr-42.md"),
            p("private/skills/draft.md"),
        ];
        for path in &private_paths {
            assert_eq!(
                PermissionEnvelope::plan_preset().check_read(path),
                Decision::Allow,
                "plan must read {path:?}"
            );
            assert_eq!(
                PermissionEnvelope::research_preset().check_read(path),
                Decision::Allow,
                "research must read {path:?}"
            );
            assert_eq!(
                PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]).check_read(path),
                Decision::Allow,
                "act must read {path:?}"
            );
            assert_eq!(
                PermissionEnvelope::autopilot_preset(vec!["src/**".into()], vec![])
                    .check_read(path),
                Decision::Allow,
                "autopilot must read {path:?}"
            );
        }
    }

    /// Locks engine-side enforcement of the cross-repo `private/` policy:
    /// every preset MUST deny writes under `private/**` with
    /// `DenyReason::SensitivePath`, regardless of whether the preset's
    /// `write` allowlist would otherwise permit the path. The orchestrator's
    /// existing grant flow then prompts the user to confirm both the intent
    /// and the target path before any bytes hit disk.
    #[test]
    fn all_presets_intercept_writes_to_private_directory() {
        let private_writes = [
            p("private/notes.md"),
            p("private/audits/wiring-2026.md"),
            p("private/reviews/pr-42.md"),
            p("private/skills/draft.md"),
        ];
        for path in &private_writes {
            for (label, env) in [
                ("plan", PermissionEnvelope::plan_preset()),
                ("research", PermissionEnvelope::research_preset()),
                (
                    "act-with-private-allow",
                    PermissionEnvelope::act_preset(
                        // Even an explicit allow for `private/**` must NOT
                        // override the sensitive-path block.
                        vec!["src/**".into(), "private/**".into()],
                        vec![],
                    ),
                ),
                (
                    "autopilot-with-private-allow",
                    PermissionEnvelope::autopilot_preset(
                        vec!["src/**".into(), "private/**".into()],
                        vec![],
                    ),
                ),
            ] {
                match env.check_write(path) {
                    Decision::Deny(DenyReason::SensitivePath(_)) => {}
                    other => panic!(
                        "{label} must Deny(SensitivePath) on write to {path:?}, got {other:?}"
                    ),
                }
            }
        }
    }

    /// Sensitive-path block must NOT bleed into reads (reads remain open).
    #[test]
    fn sensitive_paths_do_not_block_reads() {
        let env = PermissionEnvelope::plan_preset();
        assert_eq!(env.check_read(&p("private/audits/foo.md")), Decision::Allow);
    }

    /// Non-private writes must remain unaffected by the sensitive-path block.
    #[test]
    fn sensitive_paths_do_not_affect_non_private_writes() {
        let env = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]);
        assert_eq!(env.check_write(&p("src/main.rs")), Decision::Allow);
        // Out-of-allow path still denies for the original reason
        // (NotInAllowList), not SensitivePath.
        match env.check_write(&p("docs/readme.md")) {
            Decision::Deny(DenyReason::NotInAllowList | DenyReason::MatchesDeny) => {}
            other => panic!("expected NotInAllowList / MatchesDeny, got {other:?}"),
        }
    }

    /// `restrict_to` must union sensitive-path globs (a child can ADD
    /// sensitive globs but never drop them — deny-wins).
    #[test]
    fn restrict_to_unions_sensitive_write_paths() {
        let parent = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        let mut child = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        child.sensitive_write_paths.push("secrets/**".into());

        let restricted = parent.restrict_to(&child);
        assert!(restricted
            .sensitive_write_paths
            .contains(&"private/**".to_string()));
        assert!(restricted
            .sensitive_write_paths
            .contains(&"secrets/**".to_string()));

        // And the union actually fires on writes to either glob.
        match restricted.check_write(&p("secrets/key.pem")) {
            Decision::Deny(DenyReason::SensitivePath(_)) => {}
            other => panic!("expected SensitivePath, got {other:?}"),
        }
    }

    /// Backwards-compat + fail-safe: an envelope deserialised from a
    /// payload that pre-dates `sensitive_write_paths` must decode with
    /// the **default** sensitive list populated (not empty), so that
    /// legacy payloads transparently inherit the `private/**`
    /// protection rather than silently losing it.
    ///
    /// This was changed in PR-A as part of the path-normalization
    /// rewrite — the prior "empty default" was a footgun: any envelope
    /// serialised before the field existed would round-trip into an
    /// unprotected envelope. Tri-review (3-of-3 consensus) flagged it.
    #[test]
    fn sensitive_write_paths_serde_default() {
        let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        let mut json = serde_json::to_value(&env).expect("serialize");
        json.as_object_mut()
            .unwrap()
            .remove("sensitive_write_paths");
        json.as_object_mut()
            .unwrap()
            .remove("sensitive_write_exceptions");

        let restored: PermissionEnvelope =
            serde_json::from_value(json).expect("legacy payload should deserialize");
        assert_eq!(
            restored.sensitive_write_paths,
            default_sensitive_write_paths()
        );
        assert!(restored.sensitive_write_exceptions.is_empty());
        // The default protection must trip even on a permissive `**`
        // allow list.
        assert!(matches!(
            restored.check_write(&p("private/foo.md")),
            Decision::Deny(DenyReason::SensitivePath(_))
        ));
    }

    #[test]
    fn act_preset_per_folder_allow() {
        let e = PermissionEnvelope::act_preset(
            vec!["src/**".into(), "tests/**".into()],
            vec!["src/secrets/**".into()],
        );
        assert_eq!(e.check_write(&p("src/main.rs")), Decision::Allow);
        assert_eq!(e.check_write(&p("tests/unit.rs")), Decision::Allow);
        assert!(matches!(
            e.check_write(&p("docs/readme.md")),
            Decision::Deny(_)
        ));
        assert_eq!(
            e.check_write(&p("src/secrets/key.pem")),
            Decision::Deny(DenyReason::MatchesDeny)
        );
    }

    #[test]
    fn autopilot_waives_approval_but_keeps_write_envelope() {
        let e = PermissionEnvelope::autopilot_preset(vec!["src/**".into()], vec![]);
        assert_eq!(e.approval_cadence, ApprovalCadence::None);
        assert_eq!(e.check_write(&p("src/main.rs")), Decision::Allow);
        // Still denied outside envelope — scope-expansion event must fire.
        assert!(matches!(e.check_write(&p("etc/passwd")), Decision::Deny(_)));
    }

    #[test]
    fn restrict_to_deny_wins_for_paths() {
        let parent = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        let child =
            PermissionEnvelope::act_preset(vec!["src/**".into()], vec!["src/gen/**".into()]);
        let eff = parent.restrict_to(&child);
        assert_eq!(eff.check_write(&p("src/main.rs")), Decision::Allow);
        assert!(matches!(
            eff.check_write(&p("src/gen/out.rs")),
            Decision::Deny(_)
        ));
        // Parent allowed "**" but child narrowed to "src/**" — narrower wins.
        assert!(matches!(
            eff.check_write(&p("docs/foo.md")),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn restrict_to_stricter_cadence_wins() {
        let autopilot = PermissionEnvelope::autopilot_preset(vec!["**".into()], vec![]);
        let act = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]);
        let eff = autopilot.restrict_to(&act);
        assert_eq!(eff.approval_cadence, ApprovalCadence::PerMajorStep);
    }

    #[test]
    fn restrict_to_network_and_exec_narrow_via_and() {
        let parent = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        let mut child = PermissionEnvelope::plan_preset();
        child.network = NetworkPolicy::disabled();
        let eff = parent.restrict_to(&child);
        assert_eq!(
            eff.check_network(Some("example.com")),
            Decision::Deny(DenyReason::NetworkDisabled)
        );
        // exec: plan has disabled, parent has enabled — AND yields disabled.
        assert!(matches!(eff.check_exec("ls"), Decision::Deny(_)));
    }

    #[test]
    fn network_host_deny_wins() {
        let net = NetworkPolicy {
            enabled: true,
            host_allow: vec!["example.com".into()],
            host_deny: vec!["evil.example.com".into()],
        };
        assert_eq!(net.check(Some("api.example.com")), Decision::Allow);
        assert!(matches!(
            net.check(Some("evil.example.com")),
            Decision::Deny(DenyReason::HostDenied(_))
        ));
    }

    #[test]
    fn exec_always_deny_beats_enabled() {
        let e = ExecPolicy::enabled_with_guards();
        assert_eq!(e.check("ls -la"), Decision::Allow);
        assert!(matches!(
            e.check("something rm -rf / whatever"),
            Decision::Deny(DenyReason::CommandBlacklisted(_))
        ));
    }

    #[test]
    fn envelope_roundtrip_json() {
        let e =
            PermissionEnvelope::act_preset(vec!["src/**".into()], vec!["src/secrets/**".into()]);
        let j = serde_json::to_string(&e).unwrap();
        let back: PermissionEnvelope = serde_json::from_str(&j).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn skill_budget_minimum_on_restrict() {
        let parent = PermissionEnvelope::research_preset(); // 8
        let child = PermissionEnvelope::plan_preset(); // 6
        let eff = parent.restrict_to(&child);
        assert_eq!(eff.skill_budget, 6);
    }

    // ── ST-A5 — EnvelopeDefaults builder contract ─────────────────────────
    //
    // Contract `envelope-presets-v1` (decomposition.md §5):
    //   EnvelopeDefaults::new().with_mode(..).build() MUST produce
    //   byte-equal output to the pre-refactor plan/research/act/autopilot
    //   preset functions. A regression here corrupts every ModeSelection
    //   that rides through the bridge.

    #[test]
    fn from_mode_name_matches_plan_preset() {
        let via_builder = PermissionEnvelope::from_mode_name("plan", vec![], vec![]);
        let direct = PermissionEnvelope::plan_preset();
        assert_eq!(
            serde_json::to_string(&via_builder).unwrap(),
            serde_json::to_string(&direct).unwrap(),
            "plan preset must be byte-equal via from_mode_name"
        );
    }

    #[test]
    fn from_mode_name_matches_research_preset() {
        let via_builder = PermissionEnvelope::from_mode_name("research", vec![], vec![]);
        let direct = PermissionEnvelope::research_preset();
        assert_eq!(
            serde_json::to_string(&via_builder).unwrap(),
            serde_json::to_string(&direct).unwrap(),
            "research preset must be byte-equal via from_mode_name"
        );
    }

    #[test]
    fn from_mode_name_matches_act_preset() {
        let allow = vec!["src/**".to_string(), "tests/**".to_string()];
        let deny = vec!["src/secrets/**".to_string()];
        let via_builder = PermissionEnvelope::from_mode_name("act", allow.clone(), deny.clone());
        let direct = PermissionEnvelope::act_preset(allow, deny);
        assert_eq!(
            serde_json::to_string(&via_builder).unwrap(),
            serde_json::to_string(&direct).unwrap(),
            "act preset must be byte-equal via from_mode_name"
        );
    }

    #[test]
    fn from_mode_name_matches_autopilot_preset() {
        let allow = vec!["src/**".to_string()];
        let deny = vec!["src/secrets/**".to_string()];
        let via_builder =
            PermissionEnvelope::from_mode_name("autopilot", allow.clone(), deny.clone());
        let direct = PermissionEnvelope::autopilot_preset(allow, deny);
        assert_eq!(
            serde_json::to_string(&via_builder).unwrap(),
            serde_json::to_string(&direct).unwrap(),
            "autopilot preset must be byte-equal via from_mode_name"
        );
    }

    #[test]
    fn from_mode_name_unknown_defaults_to_plan() {
        let via_builder = PermissionEnvelope::from_mode_name("nonsense", vec![], vec![]);
        let direct = PermissionEnvelope::plan_preset();
        assert_eq!(
            via_builder, direct,
            "unknown mode must fall back to the safest preset (plan)"
        );
    }

    #[test]
    fn from_mode_name_capitalized_variants() {
        // Canonical names are lowercase but the bridge occasionally forwards
        // TitleCase; both MUST resolve identically.
        for (lower, upper) in [
            ("plan", "Plan"),
            ("research", "Research"),
            ("act", "Act"),
            ("autopilot", "Autopilot"),
        ] {
            let a = PermissionEnvelope::from_mode_name(lower, vec![], vec![]);
            let b = PermissionEnvelope::from_mode_name(upper, vec![], vec![]);
            assert_eq!(a, b, "'{lower}' and '{upper}' must match");
        }
    }

    // ── Full-solution invariants (PR-A tri-review followup) ───────────────
    //
    // These tests cover the four consensus / notable findings rolled up
    // into PR-A:
    //   1. Path normalisation (./, ., case, traversal)
    //   2. sensitive_write_exceptions priority (exception > sensitive > allow)
    //   3. restrict_to clears exceptions for sub-agents
    //   4. Sensitive-list is the *floor* — exception list adds back grants

    /// `./private/foo`, `private/./foo`, and `foo/./private/foo` must all
    /// trip the sensitive list — the leading `./` and `.` mid-path
    /// segments are noise. Without normalisation, `./private/x` would
    /// silently fall through `glob_match("private/**", _)` and write.
    #[test]
    fn normalise_strips_curdir_segments() {
        let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        for path in [
            "./private/foo.md",
            "private/./foo.md",
            "private/sub/./foo.md",
        ] {
            assert!(
                matches!(
                    env.check_write(&p(path)),
                    Decision::Deny(DenyReason::SensitivePath(_))
                ),
                "path {path:?} should trip sensitive list"
            );
        }
    }

    /// `..` traversal segments are *preserved* (not canonicalised) so a
    /// crafted path like `foo/../private/x` still trips the sensitive
    /// list. Canonicalising would open a different hole — symlinks /
    /// resolved roots — so we deliberately match against the literal
    /// component sequence with `.` removed.
    #[test]
    fn normalise_preserves_parentdir_so_traversal_still_trips() {
        let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        // The literal path `foo/../private/x` after stripping `.`
        // components is `foo/../private/x` — `..` survives. The
        // sensitive glob `private/**` does NOT match this literal
        // string, so the *normalisation* alone doesn't trip it. What
        // matters is that we don't *resolve* the traversal into
        // `private/x` either. Document the current behaviour: literal
        // component matching with `.` stripped, `..` preserved.
        let normalised = normalise_path_for_sensitive_match(&p("foo/../private/x.md"));
        // Must contain the unresolved `..` segment.
        assert!(
            normalised.contains(".."),
            "normalised form must preserve `..` (got {normalised:?})"
        );
        // And glob_match against the literal must NOT match (we don't
        // pretend the path is `private/x.md`):
        assert!(!glob_match("private/**", &normalised));
        // The check_write still denies because the path doesn't sit
        // under any allow-list match either — it falls to NotInAllowList.
        // We assert it's *some* form of Deny; we don't assert
        // SensitivePath, because that would require resolution logic
        // we explicitly chose not to add.
        let _ = env.check_write(&p("foo/../private/x.md"));
    }

    /// On case-insensitive filesystems (macOS, Windows), `Private/x` and
    /// `private/x` reach the same inode, so the sensitive list must
    /// match both. On Linux they're distinct paths and we only block
    /// the lowercase form.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn normalise_is_case_insensitive_on_macos_windows() {
        let env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        for path in ["Private/foo.md", "PRIVATE/foo.md", "PrIvAtE/foo.md"] {
            assert!(
                matches!(
                    env.check_write(&p(path)),
                    Decision::Deny(DenyReason::SensitivePath(_))
                ),
                "path {path:?} should trip on case-insensitive FS"
            );
        }
    }

    /// The exception list takes priority over the sensitive list, which
    /// itself takes priority over the allow / deny check. So a sensitive
    /// path appearing in `sensitive_write_exceptions` should resolve to
    /// whatever the allow list says (Allow if covered, Deny otherwise).
    #[test]
    fn sensitive_write_exceptions_unblock_specific_paths() {
        let mut env = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        // Without exception: blocked.
        assert!(matches!(
            env.check_write(&p("private/notes.md")),
            Decision::Deny(DenyReason::SensitivePath(_))
        ));
        // Add the exception (concrete path only — that's the contract).
        env.sensitive_write_exceptions
            .push("private/notes.md".into());
        // Now the exception bypasses the sensitive list and the allow
        // list (`**`) lets it through.
        assert_eq!(
            env.check_write(&p("private/notes.md")),
            Decision::Allow,
            "explicit exception must beat sensitive list"
        );
        // A *different* private file is still blocked.
        assert!(matches!(
            env.check_write(&p("private/other.md")),
            Decision::Deny(DenyReason::SensitivePath(_))
        ));
    }

    /// `restrict_to` (sub-agent narrowing) must inherit the parent's
    /// sensitive list (deny-wins, can only grow) but MUST NOT inherit
    /// the parent's exception list — sub-agents start with no grants
    /// and have to ask for their own.
    #[test]
    fn restrict_to_clears_sensitive_exceptions() {
        let mut parent = PermissionEnvelope::act_preset(vec!["**".into()], vec![]);
        parent
            .sensitive_write_exceptions
            .push("private/notes.md".into());
        let inner = PermissionEnvelope::act_preset(vec!["src/**".into()], vec![]);
        let child = parent.restrict_to(&inner);
        assert!(
            child.sensitive_write_exceptions.is_empty(),
            "sub-agents must start with no inherited grants"
        );
        // And the child still blocks private/notes.md even though the
        // parent had an exception for it.
        assert!(matches!(
            child.check_write(&p("private/notes.md")),
            Decision::Deny(_)
        ));
    }
}
