use memory_bank_app::{ServerStartupPhase, ServerStartupState};
use std::fs;
use std::io;
use std::path::PathBuf;

pub(crate) struct StartupStateTracker {
    path: PathBuf,
    pid: u32,
    namespace: String,
}

impl StartupStateTracker {
    pub(crate) fn new(path: PathBuf, namespace: impl Into<String>) -> Self {
        Self {
            path,
            pid: std::process::id(),
            namespace: namespace.into(),
        }
    }

    pub(crate) fn begin_reindex(&self, memory_count: usize) -> Result<ReindexingGuard, io::Error> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let state = ServerStartupState {
            pid: self.pid,
            namespace: self.namespace.clone(),
            phase: ServerStartupPhase::Reindexing,
            memory_count: Some(memory_count),
        };
        let payload = serde_json::to_vec_pretty(&state).map_err(io::Error::other)?;
        fs::write(&self.path, payload)?;

        Ok(ReindexingGuard {
            path: self.path.clone(),
        })
    }
}

pub(crate) struct ReindexingGuard {
    path: PathBuf,
}

impl Drop for ReindexingGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
