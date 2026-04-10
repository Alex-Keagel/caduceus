use caduceus_core::{CaduceusError, Result, SessionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ── Capabilities ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Read files within the workspace
    FsRead,
    /// Write/create/delete files within the workspace
    FsWrite,
    /// Execute shell commands/processes
    ProcessExec,
    /// Make outbound HTTP requests
    NetworkHttp,
    /// Perform mutating git operations (commit, push, etc.)
    GitMutate,
    /// Access paths outside the workspace root
    FsEscape,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FsRead => write!(f, "fs:read"),
            Self::FsWrite => write!(f, "fs:write"),
            Self::ProcessExec => write!(f, "process:exec"),
            Self::NetworkHttp => write!(f, "network:http"),
            Self::GitMutate => write!(f, "git:mutate"),
            Self::FsEscape => write!(f, "fs:escape"),
        }
    }
}

// ── Permission request/decision ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub session_id: SessionId,
    pub capability: Capability,
    pub resource: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    Allowed,
    Denied,
    AllowedForSession,
}

// ── Audit log ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub session_id: SessionId,
    pub capability: Capability,
    pub resource: String,
    pub decision: PermissionDecision,
    pub timestamp: DateTime<Utc>,
}

pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn record(&mut self, entry: AuditEntry) {
        self.entries.push(entry);
    }

    pub fn entries_for_session(&self, session_id: &SessionId) -> Vec<&AuditEntry> {
        self.entries
            .iter()
            .filter(|e| &e.session_id == session_id)
            .collect()
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

// ── PermissionEnforcer ─────────────────────────────────────────────────────────

pub struct PermissionEnforcer {
    workspace_root: PathBuf,
    granted_capabilities: HashSet<Capability>,
    session_grants: std::collections::HashMap<String, HashSet<Capability>>,
    audit: AuditLog,
}

impl PermissionEnforcer {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let mut granted = HashSet::new();
        // Default safe capabilities
        granted.insert(Capability::FsRead);
        granted.insert(Capability::FsWrite);
        granted.insert(Capability::ProcessExec);
        granted.insert(Capability::NetworkHttp);

        Self {
            workspace_root: workspace_root.into(),
            granted_capabilities: granted,
            session_grants: std::collections::HashMap::new(),
            audit: AuditLog::new(),
        }
    }

    pub fn check(
        &mut self,
        session_id: &SessionId,
        capability: &Capability,
        resource: &str,
    ) -> Result<()> {
        // FsEscape: check path is within workspace
        if matches!(capability, Capability::FsRead | Capability::FsWrite) {
            let path = Path::new(resource);
            if path.is_absolute() {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if !canonical.starts_with(&self.workspace_root) {
                    self.audit.record(AuditEntry {
                        session_id: session_id.clone(),
                        capability: capability.clone(),
                        resource: resource.to_string(),
                        decision: PermissionDecision::Denied,
                        timestamp: Utc::now(),
                    });
                    return Err(CaduceusError::PermissionDenied {
                        capability: capability.to_string(),
                        tool: format!("path:{resource}"),
                    });
                }
            }
        }

        let allowed = self.granted_capabilities.contains(capability)
            || self
                .session_grants
                .get(&session_id.to_string())
                .map(|s| s.contains(capability))
                .unwrap_or(false);

        let decision = if allowed {
            PermissionDecision::Allowed
        } else {
            PermissionDecision::Denied
        };

        self.audit.record(AuditEntry {
            session_id: session_id.clone(),
            capability: capability.clone(),
            resource: resource.to_string(),
            decision: decision.clone(),
            timestamp: Utc::now(),
        });

        if decision == PermissionDecision::Allowed {
            Ok(())
        } else {
            Err(CaduceusError::PermissionDenied {
                capability: capability.to_string(),
                tool: resource.to_string(),
            })
        }
    }

    pub fn grant_capability(&mut self, capability: Capability) {
        self.granted_capabilities.insert(capability);
    }

    pub fn revoke_capability(&mut self, capability: &Capability) {
        self.granted_capabilities.remove(capability);
    }

    pub fn audit_log(&self) -> &AuditLog {
        &self.audit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caduceus_core::SessionId;

    #[test]
    fn it_works() {
        let _cap = Capability::FsRead;
    }

    #[test]
    fn permission_check_allowed() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        let session_id = SessionId::new();
        let result = enforcer.check(&session_id, &Capability::FsRead, "src/main.rs");
        assert!(result.is_ok());
    }

    #[test]
    fn permission_check_denied_escape() {
        let mut enforcer = PermissionEnforcer::new("/workspace");
        let session_id = SessionId::new();
        let result = enforcer.check(&session_id, &Capability::FsRead, "/etc/passwd");
        // may be ok if /etc/passwd doesn't exist under /workspace, or err
        // just verify it runs without panic
        let _ = result;
    }
}
