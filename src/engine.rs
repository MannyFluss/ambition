//! The engine: what actually answers a prompt. Ambition needs very little
//! from it, and this trait is all of it. T3 Code (`t3.rs`) is the only
//! engine today. Another one, such as a coding agent run straight from its
//! command line, is one more `impl Engine`.

use std::path::Path;
use std::time::Duration;

pub type Res<T> = Result<T, String>;

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Completed,
    Failed,
    Interrupted,
    /// The agent stopped to ask something in the engine's own window.
    NeedsInput,
    /// Still going when Ambition stopped waiting.
    Running,
    Other(String),
}

impl Status {
    pub fn word(&self) -> &str {
        match self {
            Status::Completed => "completed",
            Status::Failed => "failed",
            Status::Interrupted => "interrupted",
            Status::NeedsInput => "needs-input",
            Status::Running => "running",
            Status::Other(s) => s,
        }
    }
}

/// A thread as the engine sees it now.
#[derive(Debug, Clone, Default)]
pub struct Thread {
    pub link: String,
    pub status: String,
    pub turns: u64,
}

/// A model the engine offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// Which provider instance serves it, in the engine's terms.
    pub instance: String,
    pub provider: String,
    pub id: String,
    pub label: String,
    /// The effort option's id ("effort" or "reasoningEffort"), and its values.
    pub effort_id: String,
    pub efforts: Vec<String>,
}

/// The model a thread uses, with its effort.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Selection {
    pub instance: String,
    pub model: String,
    pub effort: Option<(String, String)>,
}

pub trait Engine: Send + Sync {
    /// "T3 Code".
    fn name(&self) -> &str;
    /// The engine's project for this folder, made if it has none. Its id.
    fn project(&self, folder: &Path) -> Res<String>;
    /// Start a thread with its first message. (thread id, run id)
    fn launch(&self, project: &str, title: &str, message: &str, model: Option<&Selection>) -> Res<(String, String)>;
    /// Send a message to a thread. The run id.
    fn send(&self, thread: &str, message: &str) -> Res<String>;
    /// Wait for a run to end, up to `limit`.
    fn wait(&self, thread: &str, run: &str, limit: Duration) -> Res<Status>;
    /// The last thing the agent said in a run.
    fn reply(&self, thread: &str, run: &str) -> Res<String>;
    /// The thread now, or None when the engine no longer has it.
    fn thread(&self, thread: &str) -> Res<Option<Thread>>;
    fn interrupt(&self, thread: &str) -> Res<()>;
    /// Put an old thread away, when a new one takes over.
    fn settle(&self, thread: &str) -> Res<()>;
    /// The models on offer, or those matching `search`, in the engine's order.
    fn models(&self, search: Option<&str>) -> Res<Vec<Model>>;
    /// The model a thread uses.
    fn model_of(&self, thread: &str) -> Res<Option<Selection>>;
    /// The model a project's new threads get.
    fn project_model(&self, project: &str) -> Res<Option<Selection>>;
    /// Switch a thread (if any) and the project's new threads to a model.
    fn use_model(&self, thread: Option<&str>, project: &str, model: &Selection) -> Res<()>;
}
