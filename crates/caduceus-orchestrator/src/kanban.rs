use caduceus_core::{CaduceusError, Result, TokenUsage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const BACKLOG_COLUMN_ID: &str = "backlog";
pub const IN_PROGRESS_COLUMN_ID: &str = "in-progress";
pub const REVIEW_COLUMN_ID: &str = "review";
pub const DONE_COLUMN_ID: &str = "done";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanBoard {
    pub id: String,
    pub name: String,
    pub columns: Vec<KanbanColumn>,
    pub cards: Vec<KanbanCard>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanColumn {
    pub id: String,
    pub name: String,
    pub card_ids: Vec<String>,
    pub wip_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanCard {
    pub id: String,
    pub title: String,
    pub description: String,
    pub column_id: String,
    pub status: CardStatus,
    pub worktree_branch: Option<String>,
    pub agent_session_id: Option<String>,
    pub dependencies: Vec<String>,
    pub auto_commit: bool,
    pub auto_pr: bool,
    pub token_usage: TokenUsage,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CardStatus {
    Todo,
    Running,
    Blocked(String),
    NeedsReview,
    Done,
    Failed(String),
}

impl KanbanBoard {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            columns: default_columns(),
            cards: Vec::new(),
            created_at: Utc::now(),
        }
    }

    pub fn add_card(&mut self, mut card: KanbanCard) -> Result<()> {
        if card.id.trim().is_empty() {
            card.id = Uuid::new_v4().to_string();
        }
        if self.cards.iter().any(|existing| existing.id == card.id) {
            return Err(CaduceusError::Other(anyhow::anyhow!(
                "card already exists: {}",
                card.id
            )));
        }
        card.column_id = BACKLOG_COLUMN_ID.to_string();
        card.status = CardStatus::Todo;
        card.completed_at = None;
        self.column_mut(BACKLOG_COLUMN_ID)?
            .card_ids
            .push(card.id.clone());
        self.cards.push(card);
        Ok(())
    }

    pub fn move_card(&mut self, card_id: &str, column_id: &str) -> Result<()> {
        self.column(column_id)?;
        let card_index = self.card_index(card_id)?;
        for column in &mut self.columns {
            column.card_ids.retain(|id| id != card_id);
        }
        self.column_mut(column_id)?
            .card_ids
            .push(card_id.to_string());
        self.cards[card_index].column_id = column_id.to_string();
        Ok(())
    }

    pub fn link_cards(&mut self, from: &str, to: &str) -> Result<()> {
        if from == to {
            return Err(CaduceusError::Other(anyhow::anyhow!(
                "cannot link a card to itself"
            )));
        }
        self.card_index(from)?;
        self.card_index(to)?;
        if self.depends_on(from, to) {
            return Err(CaduceusError::Other(anyhow::anyhow!(
                "link would create a dependency cycle"
            )));
        }
        let to_index = self.card_index(to)?;
        if !self.cards[to_index]
            .dependencies
            .iter()
            .any(|id| id == from)
        {
            self.cards[to_index].dependencies.push(from.to_string());
        }
        Ok(())
    }

    pub fn ready_cards(&self) -> Vec<&KanbanCard> {
        self.cards
            .iter()
            .filter(|card| matches!(card.status, CardStatus::Todo) && self.dependencies_done(card))
            .collect()
    }

    pub fn on_card_complete(&mut self, card_id: &str) -> Result<Vec<String>> {
        let card_index = self.card_index(card_id)?;
        self.cards[card_index].status = CardStatus::Done;
        self.cards[card_index].completed_at = Some(Utc::now());
        self.move_card(card_id, DONE_COLUMN_ID)?;

        let dependents: Vec<String> = self
            .cards
            .iter()
            .filter(|card| {
                card.dependencies
                    .iter()
                    .any(|dependency| dependency == card_id)
            })
            .map(|card| card.id.clone())
            .collect();

        let mut started = Vec::new();
        for dependent_id in dependents {
            let dependent_index = self.card_index(&dependent_id)?;
            if matches!(self.cards[dependent_index].status, CardStatus::Todo)
                && self.dependencies_done(&self.cards[dependent_index])
            {
                self.cards[dependent_index].status = CardStatus::Running;
                self.move_card(&dependent_id, IN_PROGRESS_COLUMN_ID)?;
                started.push(dependent_id);
            }
        }
        Ok(started)
    }

    pub fn serialize(&self) -> Result<String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(Into::into)
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        let board: KanbanBoard = serde_json::from_str(json)?;
        board.validate()?;
        Ok(board)
    }

    pub fn save_to_workspace(&self, workspace_root: impl AsRef<Path>) -> Result<PathBuf> {
        let path = Self::storage_path(workspace_root);
        self.save_to_path(&path)?;
        Ok(path)
    }

    pub fn load_from_workspace(workspace_root: impl AsRef<Path>) -> Result<Self> {
        Self::load_from_path(Self::storage_path(workspace_root))
    }

    pub fn load_or_new(workspace_root: impl AsRef<Path>, name: impl Into<String>) -> Result<Self> {
        let path = Self::storage_path(workspace_root);
        if path.exists() {
            Self::load_from_path(path)
        } else {
            let board = Self::new(name);
            board.save_to_path(&path)?;
            Ok(board)
        }
    }

    pub fn storage_path(workspace_root: impl AsRef<Path>) -> PathBuf {
        workspace_root
            .as_ref()
            .join(".caduceus")
            .join("kanban.json")
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = self.serialize()?;
        let temp_path = path.with_extension("json.tmp");
        fs::write(&temp_path, json)?;
        fs::rename(temp_path, path)?;
        Ok(())
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let json = fs::read_to_string(path)?;
        Self::deserialize(&json)
    }

    fn validate(&self) -> Result<()> {
        let mut column_ids = HashSet::new();
        for column in &self.columns {
            if !column_ids.insert(column.id.as_str()) {
                return Err(CaduceusError::Other(anyhow::anyhow!(
                    "duplicate column id: {}",
                    column.id
                )));
            }
        }

        let card_ids: HashSet<&str> = self.cards.iter().map(|card| card.id.as_str()).collect();
        if card_ids.len() != self.cards.len() {
            return Err(CaduceusError::Other(anyhow::anyhow!("duplicate card id")));
        }

        for card in &self.cards {
            if !column_ids.contains(card.column_id.as_str()) {
                return Err(CaduceusError::Other(anyhow::anyhow!(
                    "card references unknown column: {}",
                    card.column_id
                )));
            }
            for dependency in &card.dependencies {
                if !card_ids.contains(dependency.as_str()) {
                    return Err(CaduceusError::Other(anyhow::anyhow!(
                        "card references unknown dependency: {}",
                        dependency
                    )));
                }
            }
        }

        for column in &self.columns {
            for card_id in &column.card_ids {
                if !card_ids.contains(card_id.as_str()) {
                    return Err(CaduceusError::Other(anyhow::anyhow!(
                        "column references unknown card: {}",
                        card_id
                    )));
                }
            }
        }

        for card in &self.cards {
            let containing_columns: Vec<&KanbanColumn> = self
                .columns
                .iter()
                .filter(|column| column.card_ids.iter().any(|card_id| card_id == &card.id))
                .collect();
            if containing_columns.is_empty() {
                return Err(CaduceusError::Other(anyhow::anyhow!(
                    "card {} is not present in any column",
                    card.id
                )));
            }
            if containing_columns.len() > 1 {
                return Err(CaduceusError::Other(anyhow::anyhow!(
                    "card {} is present in multiple columns",
                    card.id
                )));
            }
            if containing_columns[0].id != card.column_id {
                return Err(CaduceusError::Other(anyhow::anyhow!(
                    "card {} column mismatch: expected {}, found {}",
                    card.id,
                    card.column_id,
                    containing_columns[0].id
                )));
            }
        }

        Ok(())
    }

    fn depends_on(&self, start: &str, target: &str) -> bool {
        let mut stack = vec![start.to_string()];
        let mut visited = HashSet::new();
        while let Some(card_id) = stack.pop() {
            if !visited.insert(card_id.clone()) {
                continue;
            }
            if card_id == target {
                return true;
            }
            if let Some(card) = self.cards.iter().find(|card| card.id == card_id) {
                stack.extend(card.dependencies.iter().cloned());
            }
        }
        false
    }

    fn dependencies_done(&self, card: &KanbanCard) -> bool {
        card.dependencies.iter().all(|dependency_id| {
            self.cards
                .iter()
                .find(|candidate| candidate.id == *dependency_id)
                .map(|dependency| matches!(dependency.status, CardStatus::Done))
                .unwrap_or(false)
        })
    }

    fn column(&self, column_id: &str) -> Result<&KanbanColumn> {
        self.columns
            .iter()
            .find(|column| column.id == column_id)
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("unknown column: {column_id}")))
    }

    fn column_mut(&mut self, column_id: &str) -> Result<&mut KanbanColumn> {
        self.columns
            .iter_mut()
            .find(|column| column.id == column_id)
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("unknown column: {column_id}")))
    }

    fn card_index(&self, card_id: &str) -> Result<usize> {
        self.cards
            .iter()
            .position(|card| card.id == card_id)
            .ok_or_else(|| CaduceusError::Other(anyhow::anyhow!("unknown card: {card_id}")))
    }
}

impl KanbanCard {
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            title: title.into(),
            description: description.into(),
            column_id: BACKLOG_COLUMN_ID.to_string(),
            status: CardStatus::Todo,
            worktree_branch: None,
            agent_session_id: None,
            dependencies: Vec::new(),
            auto_commit: false,
            auto_pr: false,
            token_usage: TokenUsage::default(),
            created_at: Utc::now(),
            completed_at: None,
        }
    }
}

fn default_columns() -> Vec<KanbanColumn> {
    vec![
        KanbanColumn {
            id: BACKLOG_COLUMN_ID.to_string(),
            name: "Backlog".to_string(),
            card_ids: Vec::new(),
            wip_limit: None,
        },
        KanbanColumn {
            id: IN_PROGRESS_COLUMN_ID.to_string(),
            name: "In Progress".to_string(),
            card_ids: Vec::new(),
            wip_limit: Some(3),
        },
        KanbanColumn {
            id: REVIEW_COLUMN_ID.to_string(),
            name: "Review".to_string(),
            card_ids: Vec::new(),
            wip_limit: None,
        },
        KanbanColumn {
            id: DONE_COLUMN_ID.to_string(),
            name: "Done".to_string(),
            card_ids: Vec::new(),
            wip_limit: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_board_has_default_columns() {
        let board = KanbanBoard::new("Roadmap");
        assert_eq!(board.columns.len(), 4);
        assert_eq!(board.columns[0].name, "Backlog");
    }

    #[test]
    fn add_card_puts_it_in_backlog() {
        let mut board = KanbanBoard::new("Roadmap");
        let card = KanbanCard::new("Implement checkpointing", "");
        let card_id = card.id.clone();
        board.add_card(card).unwrap();

        assert_eq!(board.cards.len(), 1);
        assert!(board.columns[0].card_ids.contains(&card_id));
        assert_eq!(board.cards[0].column_id, BACKLOG_COLUMN_ID);
    }

    #[test]
    fn move_card_updates_columns() {
        let mut board = KanbanBoard::new("Roadmap");
        let card = KanbanCard::new("Implement checkpointing", "");
        let card_id = card.id.clone();
        board.add_card(card).unwrap();

        board.move_card(&card_id, IN_PROGRESS_COLUMN_ID).unwrap();

        assert!(board.columns[0].card_ids.is_empty());
        assert!(board.columns[1].card_ids.contains(&card_id));
        assert_eq!(board.cards[0].column_id, IN_PROGRESS_COLUMN_ID);
    }

    #[test]
    fn link_cards_makes_downstream_card_ready_only_after_dependency_completes() {
        let mut board = KanbanBoard::new("Roadmap");
        let first = KanbanCard::new("Checkpointing", "");
        let first_id = first.id.clone();
        let second = KanbanCard::new("Kanban UI", "");
        let second_id = second.id.clone();
        board.add_card(first).unwrap();
        board.add_card(second).unwrap();
        board.link_cards(&first_id, &second_id).unwrap();

        let ready_before: Vec<String> = board
            .ready_cards()
            .into_iter()
            .map(|card| card.id.clone())
            .collect();
        assert!(ready_before.contains(&first_id));
        assert!(!ready_before.contains(&second_id));

        let auto_started = board.on_card_complete(&first_id).unwrap();
        assert_eq!(auto_started, vec![second_id.clone()]);
        assert!(matches!(
            board
                .cards
                .iter()
                .find(|card| card.id == second_id)
                .unwrap()
                .status,
            CardStatus::Running
        ));
    }

    #[test]
    fn ready_cards_returns_cards_with_completed_dependencies() {
        let mut board = KanbanBoard::new("Roadmap");
        let dependency = KanbanCard::new("Dependency", "");
        let dependency_id = dependency.id.clone();
        let dependent = KanbanCard::new("Dependent", "");
        let dependent_id = dependent.id.clone();
        board.add_card(dependency).unwrap();
        board.add_card(dependent).unwrap();
        board.link_cards(&dependency_id, &dependent_id).unwrap();
        board.on_card_complete(&dependency_id).unwrap();

        let ready = board.ready_cards();
        assert!(ready.is_empty());
        assert!(matches!(
            board
                .cards
                .iter()
                .find(|card| card.id == dependent_id)
                .unwrap()
                .status,
            CardStatus::Running
        ));
    }

    #[test]
    fn board_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut board = KanbanBoard::new("Roadmap");
        board
            .add_card(KanbanCard::new("Checkpointing", ""))
            .unwrap();
        let saved_path = board.save_to_workspace(dir.path()).unwrap();
        let loaded = KanbanBoard::load_from_path(saved_path).unwrap();
        assert_eq!(loaded.cards.len(), 1);
        assert_eq!(loaded.columns[0].card_ids.len(), 1);
    }
}
