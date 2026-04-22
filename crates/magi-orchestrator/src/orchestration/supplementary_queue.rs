use std::collections::HashMap;

pub struct SupplementaryInstruction {
    pub id: String,
    pub index: usize,
    pub content: String,
    pub timestamp: u64,
    pub target_worker: Option<String>,
}

pub struct SupplementaryInstructionQueue {
    instructions: Vec<SupplementaryInstruction>,
    instruction_index: usize,
    cursors: HashMap<String, usize>,
}

impl SupplementaryInstructionQueue {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            instruction_index: 0,
            cursors: HashMap::new(),
        }
    }

    pub fn inject(
        &mut self,
        content: &str,
        is_running: bool,
        target_worker: Option<String>,
    ) -> bool {
        if !is_running {
            return false;
        }
        let trimmed = content.trim().to_string();
        if trimmed.is_empty() {
            return false;
        }
        self.instruction_index += 1;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        self.instructions.push(SupplementaryInstruction {
            id: format!("supp-{}-{}", now, self.instruction_index),
            index: self.instruction_index,
            content: trimmed,
            timestamp: now,
            target_worker,
        });
        true
    }

    pub fn consume(&mut self, worker_id: Option<&str>) -> Vec<String> {
        if self.instructions.is_empty() {
            return Vec::new();
        }

        let Some(worker_id) = worker_id else {
            let contents: Vec<String> =
                self.instructions.iter().map(|i| i.content.clone()).collect();
            self.instructions.clear();
            self.cursors.clear();
            return contents;
        };

        let last_index = self.cursors.get(worker_id).copied().unwrap_or(0);
        let pending: Vec<&SupplementaryInstruction> = self
            .instructions
            .iter()
            .filter(|i| {
                i.index > last_index
                    && (i.target_worker.is_none()
                        || i.target_worker.as_deref() == Some(worker_id))
            })
            .collect();

        if pending.is_empty() {
            return Vec::new();
        }

        let latest_index = pending.last().unwrap().index;
        self.cursors.insert(worker_id.to_string(), latest_index);
        let result: Vec<String> = pending.iter().map(|i| i.content.clone()).collect();
        self.prune();
        result
    }

    pub fn pending_count(&self) -> usize {
        self.instructions.len()
    }

    pub fn peek_pending_contents(&self) -> Vec<String> {
        self.instructions.iter().map(|i| i.content.clone()).collect()
    }

    pub fn reset(&mut self) {
        self.instructions.clear();
        self.instruction_index = 0;
        self.cursors.clear();
    }

    pub fn build_supplementary_text(&mut self, worker_id: &str) -> Option<String> {
        let instructions = self.consume(Some(worker_id));
        if instructions.is_empty() {
            return None;
        }
        let formatted: Vec<String> = instructions.iter().map(|i| format!("- {}", i)).collect();
        Some(format!(
            "[System] 用户补充指令：\n{}",
            formatted.join("\n")
        ))
    }

    fn prune(&mut self) {
        if self.instructions.is_empty() || self.cursors.is_empty() {
            return;
        }
        let min_broadcast_cursor = self.cursors.values().copied().min().unwrap_or(0);
        self.instructions.retain(|instruction| {
            if let Some(ref target) = instruction.target_worker {
                let cursor = self.cursors.get(target).copied().unwrap_or(0);
                cursor < instruction.index
            } else {
                min_broadcast_cursor < instruction.index
            }
        });
    }
}

impl Default for SupplementaryInstructionQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_when_not_running_fails() {
        let mut q = SupplementaryInstructionQueue::new();
        assert!(!q.inject("hello", false, None));
        assert_eq!(q.pending_count(), 0);
    }

    #[test]
    fn inject_and_consume_all() {
        let mut q = SupplementaryInstructionQueue::new();
        q.inject("first", true, None);
        q.inject("second", true, None);
        assert_eq!(q.pending_count(), 2);

        let result = q.consume(None);
        assert_eq!(result, vec!["first", "second"]);
        assert_eq!(q.pending_count(), 0);
    }

    #[test]
    fn consume_by_worker_tracks_cursor() {
        let mut q = SupplementaryInstructionQueue::new();
        q.inject("msg-1", true, None);
        q.inject("msg-2", true, None);

        let r1 = q.consume(Some("worker-a"));
        assert_eq!(r1, vec!["msg-1", "msg-2"]);

        q.inject("msg-3", true, None);
        let r2 = q.consume(Some("worker-a"));
        assert_eq!(r2, vec!["msg-3"]);

        let r3 = q.consume(Some("worker-a"));
        assert!(r3.is_empty());
    }

    #[test]
    fn targeted_instruction_delivered_to_correct_worker() {
        let mut q = SupplementaryInstructionQueue::new();
        q.inject("for-a", true, Some("worker-a".to_string()));
        q.inject("broadcast", true, None);
        q.inject("for-b", true, Some("worker-b".to_string()));

        let a = q.consume(Some("worker-a"));
        assert_eq!(a, vec!["for-a", "broadcast"]);

        // broadcast 已被 prune（worker-a 已消费），worker-b 只收到定向消息
        let b = q.consume(Some("worker-b"));
        assert_eq!(b, vec!["for-b"]);
    }

    #[test]
    fn prune_removes_consumed() {
        let mut q = SupplementaryInstructionQueue::new();
        q.inject("old", true, None);
        q.inject("new", true, None);

        q.consume(Some("w1"));
        assert_eq!(q.pending_count(), 0);
    }

    #[test]
    fn peek_does_not_consume() {
        let mut q = SupplementaryInstructionQueue::new();
        q.inject("msg", true, None);
        let peeked = q.peek_pending_contents();
        assert_eq!(peeked, vec!["msg"]);
        assert_eq!(q.pending_count(), 1);
    }

    #[test]
    fn reset_clears_all() {
        let mut q = SupplementaryInstructionQueue::new();
        q.inject("a", true, None);
        q.inject("b", true, None);
        q.consume(Some("w1"));
        q.reset();
        assert_eq!(q.pending_count(), 0);
        assert!(q.consume(None).is_empty());
    }

    #[test]
    fn build_supplementary_text_formats() {
        let mut q = SupplementaryInstructionQueue::new();
        q.inject("fix bug", true, None);
        q.inject("add test", true, None);
        let text = q.build_supplementary_text("w1").unwrap();
        assert!(text.contains("fix bug"));
        assert!(text.contains("add test"));
        assert!(text.starts_with("[System]"));
    }

    #[test]
    fn build_supplementary_text_none_when_empty() {
        let mut q = SupplementaryInstructionQueue::new();
        assert!(q.build_supplementary_text("w1").is_none());
    }
}
