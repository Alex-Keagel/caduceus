use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct LamportTimestamp(pub u64);

impl LamportTimestamp {
    pub fn zero() -> Self {
        Self(0)
    }

    pub fn tick(&mut self) -> Self {
        self.0 += 1;
        *self
    }

    pub fn merge(&mut self, remote: Self) {
        self.0 = self.0.max(remote.0);
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct ReplicaId(pub u16);

impl ReplicaId {
    pub const HOST: Self = Self(0);
    pub const HUMAN: Self = Self(1);
    pub const FIRST_AGENT: Self = Self(2);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    pub replica_id: ReplicaId,
    pub timestamp: LamportTimestamp,
}

impl Clock {
    pub fn new(replica_id: ReplicaId) -> Self {
        Self {
            replica_id,
            timestamp: LamportTimestamp::zero(),
        }
    }

    pub fn tick(&mut self) -> FragmentId {
        FragmentId {
            timestamp: self.timestamp.tick(),
            replica_id: self.replica_id,
        }
    }

    pub fn merge(&mut self, remote: LamportTimestamp) {
        self.timestamp.merge(remote);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FragmentId {
    pub timestamp: LamportTimestamp,
    pub replica_id: ReplicaId,
}

impl FragmentId {
    pub fn new(timestamp: LamportTimestamp, replica_id: ReplicaId) -> Self {
        Self {
            timestamp,
            replica_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FragmentPosition {
    pub fragment_id: FragmentId,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub fragment_id: Option<FragmentId>,
    pub offset: usize,
}

impl Anchor {
    pub fn start() -> Self {
        Self {
            fragment_id: None,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragmentSpan {
    pub fragment_id: FragmentId,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FragmentRange {
    pub spans: Vec<FragmentSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fragment {
    pub id: FragmentId,
    pub text: String,
    pub deleted: bool,
    pub visible_len: usize,
    pub start: usize,
    pub after_id: Option<FragmentPosition>,
    delete_ops: Vec<FragmentId>,
}

impl Fragment {
    pub fn new(
        id: FragmentId,
        text: impl Into<String>,
        after_id: Option<FragmentPosition>,
    ) -> Self {
        let text = text.into();
        let visible_len = text.chars().count();
        Self {
            id,
            text,
            deleted: false,
            visible_len,
            start: 0,
            after_id,
            delete_ops: Vec::new(),
        }
    }

    pub fn end(&self) -> usize {
        self.start + self.text.chars().count()
    }

    fn sync_visibility(&mut self, insert_undone: bool, delete_is_active: bool) {
        self.deleted = insert_undone || delete_is_active;
        self.visible_len = if self.deleted {
            0
        } else {
            self.text.chars().count()
        };
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Insert {
        id: FragmentId,
        after_id: Option<FragmentPosition>,
        text: String,
    },
    Delete {
        id: FragmentId,
        range: FragmentRange,
    },
    Undo {
        id: FragmentId,
        op_id: FragmentId,
    },
}

impl Operation {
    pub fn id(&self) -> &FragmentId {
        match self {
            Self::Insert { id, .. } | Self::Delete { id, .. } | Self::Undo { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VersionVector {
    values: HashMap<ReplicaId, LamportTimestamp>,
}

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, replica_id: ReplicaId) -> LamportTimestamp {
        self.values.get(&replica_id).copied().unwrap_or_default()
    }

    pub fn observe(&mut self, id: &FragmentId) {
        let entry = self.values.entry(id.replica_id).or_default();
        entry.merge(id.timestamp);
    }

    pub fn observed(&self, id: &FragmentId) -> bool {
        self.get(id.replica_id).0 >= id.timestamp.0
    }

    pub fn join(&mut self, other: &Self) {
        for (replica_id, timestamp) in &other.values {
            let entry = self.values.entry(*replica_id).or_default();
            entry.merge(*timestamp);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (ReplicaId, LamportTimestamp)> + '_ {
        self.values
            .iter()
            .map(|(replica, timestamp)| (*replica, *timestamp))
    }
}

#[derive(Debug, Clone)]
enum OperationRecord {
    Insert,
    Delete,
    Undo,
}

pub struct Buffer {
    fragments: Vec<Fragment>,
    version: VersionVector,
    clocks: HashMap<ReplicaId, Clock>,
    applied_ops: HashMap<FragmentId, OperationRecord>,
    undo_counts: HashMap<FragmentId, usize>,
    deferred_ops: Vec<Operation>,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            fragments: Vec::new(),
            version: VersionVector::new(),
            clocks: HashMap::new(),
            applied_ops: HashMap::new(),
            undo_counts: HashMap::new(),
            deferred_ops: Vec::new(),
        }
    }

    pub fn with_text(text: impl Into<String>) -> Self {
        let mut buffer = Self::new();
        let text = text.into();
        if !text.is_empty() {
            let _ = buffer.insert(0, text, ReplicaId::HOST);
        }
        buffer
    }

    pub fn version(&self) -> &VersionVector {
        &self.version
    }

    pub fn fragments(&self) -> &[Fragment] {
        &self.fragments
    }

    pub fn insert(
        &mut self,
        offset: usize,
        text: impl Into<String>,
        replica_id: ReplicaId,
    ) -> Operation {
        assert!(offset <= self.len(), "insert offset out of bounds");
        let id = self.next_id(replica_id);
        let after_id = self.position_before_offset(offset);
        let op = Operation::Insert {
            id,
            after_id,
            text: text.into(),
        };
        self.apply_local(op.clone());
        op
    }

    pub fn delete(&mut self, start: usize, len: usize, replica_id: ReplicaId) -> Operation {
        assert!(start <= self.len(), "delete start out of bounds");
        assert!(start + len <= self.len(), "delete range out of bounds");
        let id = self.next_id(replica_id);
        let range = self.visible_range_to_fragment_range(start, len);
        let op = Operation::Delete { id, range };
        self.apply_local(op.clone());
        op
    }

    pub fn undo(&mut self, op_id: FragmentId, replica_id: ReplicaId) -> Operation {
        assert!(
            self.applied_ops.contains_key(&op_id),
            "cannot undo unknown op"
        );
        let id = self.next_id(replica_id);
        let op = Operation::Undo { id, op_id };
        self.apply_local(op.clone());
        op
    }

    pub fn apply_remote(&mut self, op: Operation) {
        if self.applied_ops.contains_key(op.id()) {
            return;
        }

        if self.can_apply(&op) {
            self.observe_remote_clock(op.id());
            self.apply_operation(op);
            self.flush_deferred();
        } else if !self
            .deferred_ops
            .iter()
            .any(|deferred| deferred.id() == op.id())
        {
            self.deferred_ops.push(op);
        }
    }

    pub fn anchor_at(&self, offset: usize) -> Anchor {
        assert!(offset <= self.len(), "anchor offset out of bounds");
        if offset == 0 {
            return Anchor::start();
        }

        let mut visible = 0;
        for fragment in &self.fragments {
            if fragment.deleted {
                continue;
            }
            if visible + fragment.visible_len >= offset {
                return Anchor {
                    fragment_id: Some(fragment.id.clone()),
                    offset: fragment.start + (offset - visible),
                };
            }
            visible += fragment.visible_len;
        }

        let last_visible = self
            .fragments
            .iter()
            .rev()
            .find(|fragment| !fragment.deleted);
        match last_visible {
            Some(fragment) => Anchor {
                fragment_id: Some(fragment.id.clone()),
                offset: fragment.end(),
            },
            None => Anchor::start(),
        }
    }

    pub fn offset_of(&self, anchor: &Anchor) -> usize {
        let Some(anchor_id) = &anchor.fragment_id else {
            return 0;
        };

        let mut visible = 0;
        for fragment in &self.fragments {
            if &fragment.id != anchor_id {
                visible += fragment.visible_len;
                continue;
            }

            if anchor.offset <= fragment.start {
                return visible;
            }

            if anchor.offset >= fragment.end() {
                visible += fragment.visible_len;
                if anchor.offset == fragment.end() {
                    return visible;
                }
                continue;
            }

            if !fragment.deleted {
                visible += anchor.offset - fragment.start;
            }
            return visible;
        }

        visible.min(self.len())
    }

    pub fn text(&self) -> String {
        self.fragments
            .iter()
            .filter(|fragment| !fragment.deleted)
            .map(|fragment| fragment.text.as_str())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.fragments
            .iter()
            .map(|fragment| fragment.visible_len)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn apply_local(&mut self, op: Operation) {
        self.apply_operation(op);
        self.flush_deferred();
    }

    fn apply_operation(&mut self, op: Operation) {
        let op_id = op.id().clone();
        match &op {
            Operation::Insert { id, after_id, text } => {
                self.apply_insert(id.clone(), after_id.clone(), text.clone());
                self.applied_ops
                    .insert(op_id.clone(), OperationRecord::Insert);
            }
            Operation::Delete { id, range } => {
                self.apply_delete(id.clone(), range.clone());
                self.applied_ops
                    .insert(op_id.clone(), OperationRecord::Delete);
            }
            Operation::Undo {
                op_id: target_op_id,
                ..
            } => {
                self.apply_undo(target_op_id.clone());
                self.applied_ops
                    .insert(op_id.clone(), OperationRecord::Undo);
            }
        }
        self.version.observe(&op_id);
        self.recompute_visibility();
    }

    fn apply_insert(&mut self, id: FragmentId, after_id: Option<FragmentPosition>, text: String) {
        let fragment = Fragment::new(id.clone(), text, after_id.clone());
        let index = self.insertion_index(after_id, &id);
        self.fragments.insert(index, fragment);
    }

    fn apply_delete(&mut self, delete_id: FragmentId, range: FragmentRange) {
        for span in range.spans {
            self.split_fragment_boundary(&span.fragment_id, span.start);
            self.split_fragment_boundary(&span.fragment_id, span.end);
            for fragment in self.fragments.iter_mut().filter(|fragment| {
                fragment.id == span.fragment_id
                    && fragment.start >= span.start
                    && fragment.end() <= span.end
            }) {
                if !fragment.delete_ops.contains(&delete_id) {
                    fragment.delete_ops.push(delete_id.clone());
                }
            }
        }
    }

    fn apply_undo(&mut self, target_op: FragmentId) {
        *self.undo_counts.entry(target_op).or_insert(0) += 1;
    }

    fn recompute_visibility(&mut self) {
        let undo_counts = self.undo_counts.clone();
        for fragment in &mut self.fragments {
            let insert_undone = undo_counts.get(&fragment.id).copied().unwrap_or(0) % 2 == 1;
            let delete_active = fragment
                .delete_ops
                .iter()
                .any(|delete_id| undo_counts.get(delete_id).copied().unwrap_or(0) % 2 == 0);
            fragment.sync_visibility(insert_undone, delete_active);
        }
    }

    fn can_apply(&self, op: &Operation) -> bool {
        match op {
            Operation::Insert { after_id, .. } => after_id
                .as_ref()
                .map(|position| self.has_insertion(&position.fragment_id))
                .unwrap_or(true),
            Operation::Delete { range, .. } => range
                .spans
                .iter()
                .all(|span| self.has_insertion(&span.fragment_id)),
            Operation::Undo { op_id, .. } => self.applied_ops.contains_key(op_id),
        }
    }

    fn has_insertion(&self, fragment_id: &FragmentId) -> bool {
        self.fragments
            .iter()
            .any(|fragment| &fragment.id == fragment_id)
            || matches!(
                self.applied_ops.get(fragment_id),
                Some(OperationRecord::Insert)
            )
    }

    fn flush_deferred(&mut self) {
        loop {
            let mut progress = false;
            let mut remaining = Vec::new();
            let deferred = std::mem::take(&mut self.deferred_ops);
            for op in deferred {
                if self.applied_ops.contains_key(op.id()) {
                    progress = true;
                    continue;
                }
                if self.can_apply(&op) {
                    self.observe_remote_clock(op.id());
                    self.apply_operation(op);
                    progress = true;
                } else {
                    remaining.push(op);
                }
            }
            self.deferred_ops = remaining;
            if !progress {
                break;
            }
        }
    }

    fn next_id(&mut self, replica_id: ReplicaId) -> FragmentId {
        self.clocks
            .entry(replica_id)
            .or_insert_with(|| Clock::new(replica_id))
            .tick()
    }

    fn observe_remote_clock(&mut self, op_id: &FragmentId) {
        self.clocks
            .entry(op_id.replica_id)
            .or_insert_with(|| Clock::new(op_id.replica_id))
            .merge(op_id.timestamp);
    }

    fn visible_range_to_fragment_range(&mut self, start: usize, len: usize) -> FragmentRange {
        if len == 0 {
            return FragmentRange::default();
        }

        let end = start + len;
        self.split_visible_boundary(start);
        self.split_visible_boundary(end);

        let mut visible = 0;
        let mut spans = Vec::new();
        for fragment in &self.fragments {
            if fragment.deleted {
                continue;
            }
            let fragment_start = visible;
            let fragment_end = visible + fragment.visible_len;
            if fragment_end <= start {
                visible = fragment_end;
                continue;
            }
            if fragment_start >= end {
                break;
            }
            spans.push(FragmentSpan {
                fragment_id: fragment.id.clone(),
                start: fragment.start,
                end: fragment.end(),
            });
            visible = fragment_end;
        }

        FragmentRange { spans }
    }

    fn split_visible_boundary(&mut self, offset: usize) {
        if offset == 0 || offset == self.len() {
            return;
        }

        let mut visible = 0;
        let mut target = None;
        for fragment in &self.fragments {
            if fragment.deleted {
                continue;
            }
            let next_visible = visible + fragment.visible_len;
            if offset > visible && offset < next_visible {
                target = Some((fragment.id.clone(), fragment.start + (offset - visible)));
                break;
            }
            visible = next_visible;
        }

        if let Some((fragment_id, split_offset)) = target {
            self.split_fragment_boundary(&fragment_id, split_offset);
        }
    }

    fn split_fragment_boundary(&mut self, fragment_id: &FragmentId, boundary: usize) {
        let Some(index) = self.fragments.iter().position(|fragment| {
            &fragment.id == fragment_id && fragment.start < boundary && boundary < fragment.end()
        }) else {
            return;
        };

        let fragment = self.fragments[index].clone();
        let split_at = boundary - fragment.start;
        let (left_text, right_text) = split_text_at_char(&fragment.text, split_at);

        let mut left = fragment.clone();
        left.text = left_text;
        left.visible_len = if left.deleted {
            0
        } else {
            left.text.chars().count()
        };

        let mut right = fragment;
        right.start = boundary;
        right.text = right_text;
        right.visible_len = if right.deleted {
            0
        } else {
            right.text.chars().count()
        };

        self.fragments[index] = left;
        self.fragments.insert(index + 1, right);
    }

    fn position_before_offset(&self, offset: usize) -> Option<FragmentPosition> {
        let anchor = self.anchor_at(offset);
        anchor.fragment_id.map(|fragment_id| FragmentPosition {
            fragment_id,
            offset: anchor.offset,
        })
    }

    fn insertion_index(
        &mut self,
        after_id: Option<FragmentPosition>,
        new_id: &FragmentId,
    ) -> usize {
        match after_id {
            None => {
                let mut index = 0;
                while index < self.fragments.len()
                    && self.fragments[index].after_id.is_none()
                    && self.fragments[index].id > *new_id
                {
                    index += 1;
                }
                index
            }
            Some(position) => {
                self.split_fragment_boundary(&position.fragment_id, position.offset);
                let mut index = self
                    .fragments
                    .iter()
                    .position(|fragment| {
                        fragment.id == position.fragment_id && fragment.end() == position.offset
                    })
                    .map(|index| index + 1)
                    .unwrap_or(self.fragments.len());

                while index < self.fragments.len()
                    && self.fragments[index].after_id.as_ref() == Some(&position)
                    && self.fragments[index].id > *new_id
                {
                    index += 1;
                }
                index
            }
        }
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

fn split_text_at_char(text: &str, offset: usize) -> (String, String) {
    if offset == 0 {
        return (String::new(), text.to_string());
    }

    let total = text.chars().count();
    if offset >= total {
        return (text.to_string(), String::new());
    }

    let byte_index = text
        .char_indices()
        .nth(offset)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    (
        text[..byte_index].to_string(),
        text[byte_index..].to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_insert_and_text() {
        let mut buffer = Buffer::new();
        let op = buffer.insert(0, "hello", ReplicaId::HOST);
        assert!(matches!(op, Operation::Insert { .. }));
        assert_eq!(buffer.text(), "hello");
        assert_eq!(buffer.len(), 5);
        assert!(!buffer.is_empty());
    }

    #[test]
    fn insert_at_beginning_middle_and_end() {
        let mut buffer = Buffer::new();
        buffer.insert(0, "ace", ReplicaId::HOST);
        buffer.insert(1, "b", ReplicaId::HOST);
        buffer.insert(3, "d", ReplicaId::HOST);
        buffer.insert(5, "f", ReplicaId::HOST);
        assert_eq!(buffer.text(), "abcdef");
    }

    #[test]
    fn delete_marks_tombstones() {
        let mut buffer = Buffer::new();
        buffer.insert(0, "hello", ReplicaId::HOST);
        let delete = buffer.delete(1, 3, ReplicaId::HUMAN);
        assert!(matches!(delete, Operation::Delete { .. }));
        assert_eq!(buffer.text(), "ho");
        assert!(buffer.fragments.iter().any(|fragment| fragment.deleted));
    }

    #[test]
    fn undo_restores_deleted_text() {
        let mut buffer = Buffer::new();
        buffer.insert(0, "hello", ReplicaId::HOST);
        let delete = buffer.delete(1, 3, ReplicaId::HUMAN);
        let delete_id = delete.id().clone();
        buffer.undo(delete_id, ReplicaId::FIRST_AGENT);
        assert_eq!(buffer.text(), "hello");
    }

    #[test]
    fn undo_insert_hides_text() {
        let mut buffer = Buffer::new();
        let insert = buffer.insert(0, "hello", ReplicaId::HOST);
        let insert_id = insert.id().clone();
        buffer.undo(insert_id, ReplicaId::HUMAN);
        assert_eq!(buffer.text(), "");
        assert!(buffer.is_empty());
    }

    #[test]
    fn concurrent_inserts_resolve_deterministically() {
        let mut left = Buffer::new();
        let mut right = Buffer::new();

        let left_insert = left.insert(0, "A", ReplicaId::HUMAN);
        let right_insert = right.insert(0, "B", ReplicaId::FIRST_AGENT);

        left.apply_remote(right_insert.clone());
        right.apply_remote(left_insert.clone());

        assert_eq!(left.text(), "BA");
        assert_eq!(right.text(), "BA");
    }

    #[test]
    fn anchor_survives_concurrent_insert() {
        let mut buffer = Buffer::new();
        buffer.insert(0, "ac", ReplicaId::HOST);
        let anchor = buffer.anchor_at(1);

        let remote = Operation::Insert {
            id: FragmentId::new(LamportTimestamp(1), ReplicaId::FIRST_AGENT),
            after_id: None,
            text: "z".into(),
        };
        buffer.apply_remote(remote);

        assert_eq!(buffer.text(), "zac");
        assert_eq!(buffer.offset_of(&anchor), 2);
    }

    #[test]
    fn version_vector_tracking() {
        let mut buffer = Buffer::new();
        let insert = buffer.insert(0, "hello", ReplicaId::HOST);
        let delete = buffer.delete(0, 2, ReplicaId::HUMAN);

        assert!(buffer.version().observed(insert.id()));
        assert!(buffer.version().observed(delete.id()));
        assert_eq!(buffer.version().get(ReplicaId::HOST), LamportTimestamp(1));
        assert_eq!(buffer.version().get(ReplicaId::HUMAN), LamportTimestamp(1));
    }

    #[test]
    fn apply_remote_operation() {
        let mut source = Buffer::new();
        let op = source.insert(0, "hello", ReplicaId::HOST);

        let mut sink = Buffer::new();
        sink.apply_remote(op);

        assert_eq!(sink.text(), "hello");
        assert_eq!(sink.len(), 5);
    }

    #[test]
    fn apply_remote_out_of_order_defers_until_dependencies_exist() {
        let mut buffer = Buffer::new();
        let parent_id = FragmentId::new(LamportTimestamp(1), ReplicaId::HOST);
        let child_id = FragmentId::new(LamportTimestamp(1), ReplicaId::FIRST_AGENT);

        let child = Operation::Insert {
            id: child_id,
            after_id: Some(FragmentPosition {
                fragment_id: parent_id.clone(),
                offset: 1,
            }),
            text: "b".into(),
        };
        let parent = Operation::Insert {
            id: parent_id,
            after_id: None,
            text: "a".into(),
        };

        buffer.apply_remote(child);
        assert_eq!(buffer.text(), "");
        buffer.apply_remote(parent);

        assert_eq!(buffer.text(), "ab");
    }

    #[test]
    fn serialize_and_deserialize_operations() {
        let op = Operation::Delete {
            id: FragmentId::new(LamportTimestamp(4), ReplicaId::FIRST_AGENT),
            range: FragmentRange {
                spans: vec![FragmentSpan {
                    fragment_id: FragmentId::new(LamportTimestamp(1), ReplicaId::HOST),
                    start: 0,
                    end: 3,
                }],
            },
        };

        let encoded = serde_json::to_string(&op).unwrap();
        let decoded: Operation = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, op);
    }

    #[test]
    fn large_document_handles_many_inserts() {
        let mut buffer = Buffer::new();
        for i in 0..1000 {
            buffer.insert(buffer.len(), "x", ReplicaId((i % 8) as u16));
        }
        assert_eq!(buffer.len(), 1000);
        assert_eq!(buffer.text().chars().count(), 1000);
    }

    #[test]
    fn duplicate_remote_operations_are_idempotent() {
        let mut source = Buffer::new();
        let op = source.insert(0, "hello", ReplicaId::HOST);

        let mut sink = Buffer::new();
        sink.apply_remote(op.clone());
        sink.apply_remote(op);

        assert_eq!(sink.text(), "hello");
        assert_eq!(sink.fragments.len(), 1);
    }
}
