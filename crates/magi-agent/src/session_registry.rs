use std::collections::HashMap;

use crate::session_context::{SessionExecutionContext, SessionExecutionContextStats};

const DEFAULT_IDLE_TTL_MS: u64 = 10 * 60 * 1000;

pub struct SessionRuntimeRegistry {
    contexts: HashMap<String, SessionExecutionContext>,
    idle_ttl_ms: u64,
}

impl SessionRuntimeRegistry {
    pub fn new(idle_ttl_ms: Option<u64>) -> Self {
        Self {
            contexts: HashMap::new(),
            idle_ttl_ms: idle_ttl_ms.unwrap_or(DEFAULT_IDLE_TTL_MS),
        }
    }

    pub fn get_or_create(&mut self, session_id: &str) -> &mut SessionExecutionContext {
        let normalized = session_id.trim().to_string();
        self.contexts
            .entry(normalized.clone())
            .or_insert_with(|| SessionExecutionContext::new(normalized))
    }

    pub fn get(&self, session_id: &str) -> Option<&SessionExecutionContext> {
        self.contexts.get(session_id.trim())
    }

    pub fn get_mut(&mut self, session_id: &str) -> Option<&mut SessionExecutionContext> {
        self.contexts.get_mut(session_id.trim())
    }

    pub fn has(&self, session_id: &str) -> bool {
        self.contexts.contains_key(session_id.trim())
    }

    pub fn remove(&mut self, session_id: &str) -> bool {
        let normalized = session_id.trim();
        if let Some(ctx) = self.contexts.get_mut(normalized) {
            ctx.clear_transient_state();
        }
        self.contexts.remove(normalized).is_some()
    }

    pub fn list_ids(&self) -> Vec<String> {
        self.contexts.keys().cloned().collect()
    }

    pub fn prune_idle(&mut self) -> Vec<String> {
        let ttl = self.idle_ttl_ms;
        let idle_ids: Vec<String> = self
            .contexts
            .iter()
            .filter(|(_, ctx)| ctx.is_idle(ttl))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &idle_ids {
            if let Some(ctx) = self.contexts.get_mut(id) {
                ctx.clear_transient_state();
            }
            self.contexts.remove(id);
        }
        idle_ids
    }

    pub fn clear(&mut self) {
        for ctx in self.contexts.values_mut() {
            ctx.clear_transient_state();
        }
        self.contexts.clear();
    }

    pub fn size(&self) -> usize {
        self.contexts.len()
    }

    pub fn stats(&self) -> Vec<SessionExecutionContextStats> {
        self.contexts.values().map(|ctx| ctx.stats()).collect()
    }
}

impl Default for SessionRuntimeRegistry {
    fn default() -> Self {
        Self::new(None)
    }
}
