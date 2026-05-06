use std::collections::HashMap;
use std::time::Instant;

#[allow(dead_code)]
pub struct BackgroundTask {
    pub task_id: String,
    pub task_type: String,
    pub description: String,
    pub started_at: Instant,
}

pub struct BackgroundTaskTracker {
    tasks: HashMap<String, BackgroundTask>,
}

impl BackgroundTaskTracker {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    pub fn track_started(&mut self, task_id: &str, task_type: &str, description: &str) {
        self.tasks.insert(
            task_id.to_string(),
            BackgroundTask {
                task_id: task_id.to_string(),
                task_type: task_type.to_string(),
                description: description.to_string(),
                started_at: Instant::now(),
            },
        );
    }

    pub fn track_progress(&mut self, task_id: &str, description: &str) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.description = description.to_string();
        }
    }

    pub fn track_completed(&mut self, task_id: &str) -> Option<BackgroundTask> {
        self.tasks.remove(task_id)
    }

    #[allow(dead_code)]
    pub fn active_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn has_active_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_lifecycle() {
        let mut tracker = BackgroundTaskTracker::new();
        assert!(!tracker.has_active_tasks());
        assert_eq!(tracker.active_count(), 0);

        tracker.track_started("task-1", "local_bash", "running echo");
        assert!(tracker.has_active_tasks());
        assert_eq!(tracker.active_count(), 1);

        let completed = tracker.track_completed("task-1");
        assert!(completed.is_some());
        let task = completed.unwrap();
        assert_eq!(task.task_id, "task-1");
        assert_eq!(task.task_type, "local_bash");
        assert_eq!(task.description, "running echo");
        assert!(!tracker.has_active_tasks());
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn multiple_tasks() {
        let mut tracker = BackgroundTaskTracker::new();

        tracker.track_started("task-a", "local_bash", "cmd a");
        tracker.track_started("task-b", "local_agent", "agent b");
        tracker.track_started("task-c", "local_bash", "cmd c");

        assert_eq!(tracker.active_count(), 3);
        assert!(tracker.has_active_tasks());

        tracker.track_completed("task-b");
        assert_eq!(tracker.active_count(), 2);

        tracker.track_completed("task-a");
        tracker.track_completed("task-c");
        assert_eq!(tracker.active_count(), 0);
        assert!(!tracker.has_active_tasks());
    }

    #[test]
    fn duplicate_track_started_overwrites() {
        let mut tracker = BackgroundTaskTracker::new();

        tracker.track_started("task-1", "local_bash", "first description");
        tracker.track_started("task-1", "local_agent", "second description");

        assert_eq!(tracker.active_count(), 1);

        let completed = tracker.track_completed("task-1").unwrap();
        assert_eq!(completed.task_type, "local_agent");
        assert_eq!(completed.description, "second description");
    }

    #[test]
    fn track_completed_missing_returns_none() {
        let mut tracker = BackgroundTaskTracker::new();
        let result = tracker.track_completed("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn track_progress_updates_description() {
        let mut tracker = BackgroundTaskTracker::new();

        tracker.track_started("task-1", "local_bash", "initial");
        tracker.track_progress("task-1", "updated progress");

        let completed = tracker.track_completed("task-1").unwrap();
        assert_eq!(completed.description, "updated progress");
    }

    #[test]
    fn track_progress_on_missing_is_noop() {
        let mut tracker = BackgroundTaskTracker::new();
        // should not panic
        tracker.track_progress("nonexistent", "some description");
        assert_eq!(tracker.active_count(), 0);
    }
}
