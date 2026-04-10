use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Lamport clock ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LamportTimestamp(pub u64);

impl LamportTimestamp {
    pub fn zero() -> Self {
        Self(0)
    }

    pub fn inc(self) -> Self {
        Self(self.0 + 1)
    }

    pub fn merge(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }
}

// ── Replica ID ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplicaId(pub Uuid);

impl ReplicaId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ReplicaId {
    fn default() -> Self {
        Self::new()
    }
}

// ── Anchor (stable reference into a buffer) ────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub fragment_id: FragmentId,
    pub bias: AnchorBias,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnchorBias {
    Left,
    Right,
}

// ── Fragment ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FragmentId {
    pub timestamp: LamportTimestamp,
    pub replica: ReplicaId,
}

impl FragmentId {
    pub fn new(timestamp: LamportTimestamp, replica: ReplicaId) -> Self {
        Self { timestamp, replica }
    }
}

impl PartialOrd for FragmentId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FragmentId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.timestamp
            .cmp(&other.timestamp)
            .then_with(|| self.replica.0.cmp(&other.replica.0))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fragment {
    pub id: FragmentId,
    pub text: String,
    pub deleted: bool,
    pub deleted_at: Option<FragmentId>,
}

impl Fragment {
    pub fn new(id: FragmentId, text: impl Into<String>) -> Self {
        Self {
            id,
            text: text.into(),
            deleted: false,
            deleted_at: None,
        }
    }
}

// ── Buffer ─────────────────────────────────────────────────────────────────────

pub struct Buffer {
    replica: ReplicaId,
    clock: LamportTimestamp,
    fragments: Vec<Fragment>,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            replica: ReplicaId::new(),
            clock: LamportTimestamp::zero(),
            fragments: Vec::new(),
        }
    }

    pub fn with_text(text: impl Into<String>) -> Self {
        let mut buf = Self::new();
        buf.insert(0, text.into());
        buf
    }

    fn next_id(&mut self) -> FragmentId {
        self.clock = self.clock.inc();
        FragmentId::new(self.clock, self.replica.clone())
    }

    pub fn insert(&mut self, position: usize, text: impl Into<String>) {
        let id = self.next_id();
        let fragment = Fragment::new(id, text);

        // Find insertion point based on visible position
        let mut visible = 0;
        let mut insert_idx = self.fragments.len();
        for (i, f) in self.fragments.iter().enumerate() {
            if !f.deleted {
                if visible == position {
                    insert_idx = i;
                    break;
                }
                visible += f.text.chars().count();
            }
        }
        self.fragments.insert(insert_idx, fragment);
    }

    pub fn delete(&mut self, start: usize, len: usize) {
        let del_id = self.next_id();
        let mut visible = 0;
        let mut deleted = 0;

        for f in self.fragments.iter_mut() {
            if f.deleted {
                continue;
            }
            let char_len = f.text.chars().count();
            if visible + char_len > start && deleted < len {
                f.deleted = true;
                f.deleted_at = Some(del_id.clone());
                deleted += char_len;
            }
            visible += char_len;
            if deleted >= len {
                break;
            }
        }
    }

    pub fn text(&self) -> String {
        self.fragments
            .iter()
            .filter(|f| !f.deleted)
            .map(|f| f.text.as_str())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.fragments
            .iter()
            .filter(|f| !f.deleted)
            .map(|f| f.text.chars().count())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn replica_id(&self) -> &ReplicaId {
        &self.replica
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let buf = Buffer::new();
        assert!(buf.is_empty());
    }

    #[test]
    fn buffer_insert_delete() {
        let mut buf = Buffer::with_text("hello");
        buf.insert(1, " world");
        assert_eq!(buf.text(), "hello world");
        buf.delete(0, 1);
        assert_eq!(buf.text(), " world");
    }

    #[test]
    fn lamport_clock_ordering() {
        let a = LamportTimestamp(1);
        let b = LamportTimestamp(2);
        assert!(a < b);
        assert_eq!(a.merge(b), b);
    }
}
