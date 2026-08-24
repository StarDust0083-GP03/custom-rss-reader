use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

/// Public, non-sensitive state used by the status bar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiActivitySnapshot {
    pub version: u64,
    pub task_id: Option<u64>,
    pub phase: String,
    pub kind: String,
    pub title: Option<String>,
    pub current: Option<usize>,
    pub total: Option<usize>,
    pub candidate_count: Option<usize>,
    pub queue_length: usize,
    pub started_at_ms: Option<i64>,
}

impl Default for AiActivitySnapshot {
    fn default() -> Self {
        Self {
            version: 0,
            task_id: None,
            phase: "idle".into(),
            kind: String::new(),
            title: None,
            current: None,
            total: None,
            candidate_count: None,
            queue_length: 0,
            started_at_ms: None,
        }
    }
}

/// Description attached to one model task. It deliberately contains no
/// prompt, article body, credential, or model response.
#[derive(Debug, Clone)]
pub struct AiTaskSpec {
    pub kind: String,
    pub title: Option<String>,
    pub total: Option<usize>,
    pub candidate_count: Option<usize>,
    pub priority: bool,
}

impl AiTaskSpec {
    pub fn translation(title: Option<String>) -> Self {
        Self {
            kind: "translation".into(),
            title,
            total: None,
            candidate_count: None,
            priority: true,
        }
    }

    pub fn classification(title: Option<String>) -> Self {
        Self {
            kind: "classification".into(),
            title,
            total: None,
            candidate_count: None,
            priority: true,
        }
    }

    pub fn background_classification(total: usize) -> Self {
        Self {
            kind: "background-classification".into(),
            title: None,
            total: Some(total),
            candidate_count: None,
            priority: false,
        }
    }

    pub fn recommendations(candidate_count: usize) -> Self {
        Self {
            kind: "recommendations".into(),
            title: None,
            total: None,
            candidate_count: Some(candidate_count),
            priority: true,
        }
    }

    pub fn connection_test() -> Self {
        Self {
            kind: "connection-test".into(),
            title: None,
            total: None,
            candidate_count: None,
            priority: true,
        }
    }
}

#[derive(Debug, Clone)]
struct TaskState {
    sequence: u64,
    spec: AiTaskSpec,
    phase: &'static str,
    current: Option<usize>,
    total: Option<usize>,
    started_at_ms: i64,
}

#[derive(Debug, Default)]
struct ActivityState {
    next_task_id: u64,
    next_sequence: u64,
    version: u64,
    tasks: HashMap<u64, TaskState>,
}

struct ActivityInner {
    state: Mutex<ActivityState>,
    app: StdMutex<Option<AppHandle>>,
}

/// Shared activity store used by commands, the feed service, and the LLM gate.
#[derive(Clone)]
pub struct AiActivityStore {
    inner: Arc<ActivityInner>,
}

impl Default for AiActivityStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AiActivityStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ActivityInner {
                state: Mutex::new(ActivityState::default()),
                app: StdMutex::new(None),
            }),
        }
    }

    pub fn attach_app(&self, app: AppHandle) {
        if let Ok(mut handle) = self.inner.app.lock() {
            *handle = Some(app);
        }
    }

    pub async fn begin(&self, spec: AiTaskSpec) -> AiTaskContext {
        let priority = spec.priority;
        let (task_id, snapshot) = {
            let mut state = self.inner.state.lock().await;
            state.next_task_id += 1;
            state.next_sequence += 1;
            let task_id = state.next_task_id;
            let sequence = state.next_sequence;
            state.tasks.insert(
                task_id,
                TaskState {
                    sequence,
                    total: spec.total,
                    spec,
                    phase: "waiting",
                    current: None,
                    started_at_ms: now_ms(),
                },
            );
            let snapshot = snapshot_locked(&mut state, true);
            (task_id, snapshot)
        };
        self.emit(snapshot);
        AiTaskContext {
            store: self.clone(),
            task_id,
            priority,
        }
    }

    pub async fn finish(&self, task_id: u64) {
        let snapshot = {
            let mut state = self.inner.state.lock().await;
            if state.tasks.remove(&task_id).is_none() {
                return;
            }
            snapshot_locked(&mut state, true)
        };
        self.emit(snapshot);
    }

    async fn set_phase(&self, task_id: u64, phase: &'static str) {
        let snapshot = {
            let mut state = self.inner.state.lock().await;
            let Some(task) = state.tasks.get_mut(&task_id) else {
                return;
            };
            task.phase = phase;
            snapshot_locked(&mut state, true)
        };
        self.emit(snapshot);
    }

    async fn set_progress(&self, task_id: u64, current: usize, total: usize) {
        let snapshot = {
            let mut state = self.inner.state.lock().await;
            let Some(task) = state.tasks.get_mut(&task_id) else {
                return;
            };
            task.current = Some(current);
            task.total = Some(total);
            snapshot_locked(&mut state, true)
        };
        self.emit(snapshot);
    }

    pub async fn snapshot(&self) -> AiActivitySnapshot {
        let mut state = self.inner.state.lock().await;
        snapshot_locked(&mut state, false)
    }

    fn emit(&self, snapshot: AiActivitySnapshot) {
        let app = self.inner.app.lock().ok().and_then(|handle| handle.clone());
        if let Some(app) = app {
            let _ = app.emit_to("main", "ai-activity", snapshot);
        }
    }
}

fn snapshot_locked(state: &mut ActivityState, bump_version: bool) -> AiActivitySnapshot {
    if bump_version {
        state.version += 1;
    }

    let selected = state
        .tasks
        .iter()
        .filter(|(_, task)| task.phase == "running")
        .min_by_key(|(_, task)| task.sequence)
        .or_else(|| {
            state
                .tasks
                .iter()
                .filter(|(_, task)| task.phase == "waiting")
                .min_by_key(|(_, task)| (!task.spec.priority, task.sequence))
        });

    let waiting = state
        .tasks
        .values()
        .filter(|task| task.phase == "waiting")
        .count();

    let Some((task_id, task)) = selected else {
        return AiActivitySnapshot {
            version: state.version,
            ..AiActivitySnapshot::default()
        };
    };
    let queue_length = waiting.saturating_sub(usize::from(task.phase == "waiting"));

    AiActivitySnapshot {
        version: state.version,
        task_id: Some(*task_id),
        phase: task.phase.into(),
        kind: task.spec.kind.clone(),
        title: task.spec.title.clone(),
        current: task.current,
        total: task.total,
        candidate_count: task.spec.candidate_count,
        queue_length,
        started_at_ms: Some(task.started_at_ms),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Handle carried through the async call tree for one task.
#[derive(Clone)]
pub struct AiTaskContext {
    store: AiActivityStore,
    task_id: u64,
    priority: bool,
}

impl AiTaskContext {
    #[cfg(test)]
    pub fn task_id(&self) -> u64 {
        self.task_id
    }

    pub fn priority(&self) -> bool {
        self.priority
    }

    pub async fn waiting(&self) {
        self.store.set_phase(self.task_id, "waiting").await;
    }

    pub async fn running(&self) {
        self.store.set_phase(self.task_id, "running").await;
    }

    pub async fn progress(&self, current: usize, total: usize) {
        self.store.set_progress(self.task_id, current, total).await;
    }

    pub async fn finish(&self) {
        self.store.finish(self.task_id).await;
    }
}

// The LLM gate runs below the command layer, so task-local context is the
// smallest safe way to carry task identity into every queued request.
tokio::task_local! {
    static CURRENT_AI_TASK: AiTaskContext;
}

pub async fn with_ai_task<F, T>(context: AiTaskContext, future: F) -> T
where
    F: Future<Output = T>,
{
    CURRENT_AI_TASK.scope(context, future).await
}

pub fn current_ai_task() -> Option<AiTaskContext> {
    CURRENT_AI_TASK.try_with(Clone::clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn task_moves_from_waiting_to_running_to_idle() {
        let store = AiActivityStore::new();
        let task = store
            .begin(AiTaskSpec::translation(Some("Article".into())))
            .await;
        assert_eq!(store.snapshot().await.phase, "waiting");
        task.running().await;
        assert_eq!(store.snapshot().await.phase, "running");
        task.finish().await;
        assert_eq!(store.snapshot().await.phase, "idle");
    }

    #[tokio::test]
    async fn interactive_task_is_selected_ahead_of_background_task() {
        let store = AiActivityStore::new();
        let _background = store.begin(AiTaskSpec::background_classification(20)).await;
        let interactive = store.begin(AiTaskSpec::recommendations(8)).await;
        let snapshot = store.snapshot().await;
        assert_eq!(snapshot.task_id, Some(interactive.task_id()));
        assert_eq!(snapshot.kind, "recommendations");
        assert_eq!(snapshot.queue_length, 1);
    }

    #[tokio::test]
    async fn newer_task_survives_old_task_finishing() {
        let store = AiActivityStore::new();
        let old = store.begin(AiTaskSpec::translation(None)).await;
        let new = store.begin(AiTaskSpec::recommendations(4)).await;
        old.finish().await;
        let snapshot = store.snapshot().await;
        assert_eq!(snapshot.task_id, Some(new.task_id()));
        assert_eq!(snapshot.kind, "recommendations");
    }
}
