//! Tests for issues #236–#237, #239–#240, #245–#246.
//!
//! Relocated from `lib.rs` (ST-B1 Wave 3) to keep the main module
//! surface manageable. The `#[cfg(test)] mod feature_tests_236_246;`
//! declaration in `lib.rs` brings these tests in unchanged.

use super::*;

// ── #236 PrdParser ────────────────────────────────────────────────────────

#[test]
fn prd_extract_sections_basic() {
    let md = "# Auth\nBuild login.\n## OAuth\nUse OAuth2.";
    let sections = PrdParser::extract_sections(md);
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].0, "Auth");
    assert!(sections[0].1.contains("Build login"));
    assert_eq!(sections[1].0, "OAuth");
}

#[test]
fn prd_parse_sets_parent_id() {
    let md = "# Feature\nTop level.\n## Sub-feature\nChild task.";
    let tasks = PrdParser::parse(md);
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].parent_id, None);
    assert_eq!(tasks[1].parent_id, Some(0));
}

#[test]
fn prd_parse_extracts_priority() {
    let md = "# Task\npriority: high\nDo something.";
    let tasks = PrdParser::parse(md);
    assert_eq!(tasks[0].priority, 8);
}

#[test]
fn prd_infer_dependencies_finds_reference() {
    let tasks = vec![
        PrdTask {
            id: 0,
            title: "Database setup".to_string(),
            description: "Set up the database.".to_string(),
            parent_id: None,
            priority: 5,
            complexity: 5,
            estimated_hours: 1.0,
            dependencies: vec![],
            tags: vec![],
        },
        PrdTask {
            id: 1,
            title: "API layer".to_string(),
            description: "Implement API after Database setup is complete.".to_string(),
            parent_id: None,
            priority: 5,
            complexity: 5,
            estimated_hours: 1.0,
            dependencies: vec![],
            tags: vec![],
        },
    ];
    let deps = PrdParser::infer_dependencies(&tasks);
    assert!(deps.contains(&(1, 0)));
}

// ── #237 TaskRecommender ──────────────────────────────────────────────────

fn make_task(id: usize, priority: u8, complexity: u8, deps: Vec<usize>) -> PrdTask {
    PrdTask {
        id,
        title: format!("Task {id}"),
        description: String::new(),
        parent_id: None,
        priority,
        complexity,
        estimated_hours: 1.0,
        dependencies: deps,
        tags: vec![],
    }
}

#[test]
fn recommender_excludes_completed() {
    let tasks = vec![make_task(0, 9, 1, vec![]), make_task(1, 5, 5, vec![])];
    let recs = TaskRecommender::recommend_next(&tasks, &[0]);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].task_id, 1);
}

#[test]
fn recommender_dep_not_ready_scores_zero_component() {
    let tasks = vec![
        make_task(0, 8, 1, vec![99]), // dep 99 not completed
        make_task(1, 5, 5, vec![]),
    ];
    let recs = TaskRecommender::recommend_next(&tasks, &[]);
    // Task 1 should score higher because task 0's dep is not satisfied
    let id1 = recs.iter().find(|r| r.task_id == 1).unwrap();
    let id0 = recs.iter().find(|r| r.task_id == 0).unwrap();
    assert!(id1.score > id0.score);
}

#[test]
fn recommender_score_formula() {
    // Single task: dep_ready=1 (no deps), priority=10 -> 1.0, complexity=1 -> 1.0
    let tasks = vec![make_task(0, 10, 1, vec![])];
    let recs = TaskRecommender::recommend_next(&tasks, &[]);
    let expected = 0.4 * 1.0 + 0.35 * 1.0 + 0.25 * 1.0;
    assert!((recs[0].score - expected).abs() < 1e-9);
}

// ── #239 TaskTree ─────────────────────────────────────────────────────────

#[test]
fn task_tree_add_and_get() {
    let mut tree = TaskTree::new();
    let root = tree.add_task("Root", None);
    let child = tree.add_task("Child", Some(root));
    assert_eq!(tree.get_task(root).unwrap().title, "Root");
    assert_eq!(tree.get_task(child).unwrap().parent_id, Some(root));
}

#[test]
fn task_tree_depth() {
    let mut tree = TaskTree::new();
    let a = tree.add_task("A", None);
    let b = tree.add_task("B", Some(a));
    let c = tree.add_task("C", Some(b));
    assert_eq!(tree.depth(a), 0);
    assert_eq!(tree.depth(b), 1);
    assert_eq!(tree.depth(c), 2);
}

#[test]
fn task_tree_children_and_subtree() {
    let mut tree = TaskTree::new();
    let root = tree.add_task("Root", None);
    let c1 = tree.add_task("C1", Some(root));
    let _c2 = tree.add_task("C2", Some(root));
    let gc = tree.add_task("GC", Some(c1));
    assert_eq!(tree.children(root).len(), 2);
    let sub = tree.subtree(root);
    assert_eq!(sub.len(), 3);
    assert!(sub.iter().any(|t| t.id == gc));
}

#[test]
fn task_tree_progress() {
    let mut tree = TaskTree::new();
    let root = tree.add_task("Root", None);
    let c1 = tree.add_task("C1", Some(root));
    let c2 = tree.add_task("C2", Some(root));
    tree.tasks.get_mut(&c1).unwrap().status = "done".to_string();
    let _ = c2;
    assert!((tree.progress(root) - 50.0).abs() < 1e-9);
}

#[test]
fn task_tree_to_tree_string() {
    let mut tree = TaskTree::new();
    let root = tree.add_task("Root", None);
    tree.add_task("Child", Some(root));
    let s = tree.to_tree_string();
    assert!(s.contains("Root"));
    assert!(s.contains("Child"));
    assert!(s.contains("  -")); // indented child
}

// ── #240 TimeTracker ──────────────────────────────────────────────────────

#[test]
fn time_tracker_start_complete_velocity() {
    let mut tracker = TimeTracker::new();
    tracker.start_task(1, 4.0);
    tracker.complete_task(1, 2.0);
    // velocity = 4.0 / 2.0 = 2.0
    assert!((tracker.velocity() - 2.0).abs() < 1e-9);
}

#[test]
fn time_tracker_totals() {
    let mut tracker = TimeTracker::new();
    tracker.start_task(1, 3.0);
    tracker.complete_task(1, 2.0);
    tracker.start_task(2, 5.0);
    tracker.complete_task(2, 6.0);
    assert!((tracker.total_estimated() - 8.0).abs() < 1e-9);
    assert!((tracker.total_actual() - 8.0).abs() < 1e-9);
}

#[test]
fn time_tracker_no_completed_velocity_one() {
    let tracker = TimeTracker::new();
    assert!((tracker.velocity() - 1.0).abs() < 1e-9);
}

// ── #246 ProgressInferrer ─────────────────────────────────────────────────

#[test]
fn progress_infer_from_commits_matching() {
    let msgs = vec![
        "implement auth login".to_string(),
        "fix auth token bug".to_string(),
        "unrelated commit".to_string(),
    ];
    let p = ProgressInferrer::infer_from_commits("auth", &msgs);
    assert!(p.confidence > 0.0);
    assert_eq!(p.evidence.len(), 2);
}

#[test]
fn progress_infer_from_commits_empty() {
    let p = ProgressInferrer::infer_from_commits("auth", &[]);
    assert_eq!(p.percentage, 0.0);
    assert_eq!(p.confidence, 0.0);
}

#[test]
fn progress_infer_from_tests() {
    assert!((ProgressInferrer::infer_from_tests(10, 8) - 80.0).abs() < 1e-9);
    assert_eq!(ProgressInferrer::infer_from_tests(0, 0), 0.0);
}

#[test]
fn progress_infer_from_files() {
    assert!((ProgressInferrer::infer_from_files(4, 2) - 50.0).abs() < 1e-9);
    assert!((ProgressInferrer::infer_from_files(4, 5) - 100.0).abs() < 1e-9);
}

#[test]
fn progress_combined() {
    let c = ProgressInferrer::combined(100.0, 80.0, 60.0);
    let expected = 0.4 * 100.0 + 0.4 * 80.0 + 0.2 * 60.0;
    assert!((c - expected).abs() < 1e-9);
}
