use std::sync::Arc;

use am_agents::{AgentAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter};
use am_proto::AgentKind;

/// Registry of available agent adapters. New providers are registered here.
pub struct AgentRegistry {
    claude: Arc<ClaudeAdapter>,
    codex: Arc<CodexAdapter>,
    cursor: Arc<CursorAdapter>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            claude: Arc::new(ClaudeAdapter::new()),
            codex: Arc::new(CodexAdapter::new()),
            cursor: Arc::new(CursorAdapter::new()),
        }
    }

    /// Resolve the adapter for an agent kind, if one is implemented.
    pub fn get(&self, kind: AgentKind) -> Option<Arc<dyn AgentAdapter>> {
        match kind {
            AgentKind::ClaudeCode => Some(self.claude.clone()),
            AgentKind::Codex => Some(self.codex.clone()),
            AgentKind::Cursor => Some(self.cursor.clone()),
            _ => None,
        }
    }

    /// Implemented adapters in the order they should appear in settings.
    pub fn implemented(&self) -> Vec<Arc<dyn AgentAdapter>> {
        vec![self.claude.clone(), self.codex.clone(), self.cursor.clone()]
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}
