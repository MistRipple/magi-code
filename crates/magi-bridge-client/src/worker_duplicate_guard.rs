use magi_permissions::PermissionEngine;
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};

const FAILED_WRITE_CACHE_TTL_MS: u64 = 600_000;
const SUCCESS_WRITE_CACHE_TTL_MS: u64 = 600_000;
const READ_ONLY_DEDUP_TTL_MS: u64 = 600_000;

/// 通过 PermissionEngine 的内置只读工具表共用一份判定，避免本地再维护副本。
static PERMISSION_ENGINE: Lazy<PermissionEngine> =
    Lazy::new(PermissionEngine::with_builtin_defaults);

fn write_dedup_tool_names() -> HashSet<&'static str> {
    [
        "shell_exec",
        "file_create",
        "file_edit",
        "file_insert",
        "file_remove",
    ]
    .into_iter()
    .collect()
}

#[derive(Clone, Debug)]
pub struct ToolCallInfo {
    pub name: String,
    pub arguments: serde_json::Value,
}

struct ReadOnlyCacheEntry {
    count: u32,
    _first_at: u64,
    last_at: u64,
}

struct FailedWriteEntry {
    count: u32,
    error: String,
    _first_at: u64,
    last_at: u64,
}

struct SuccessWriteEntry {
    file_path: String,
    updated_at: u64,
}

pub struct WorkerDuplicateGuard {
    read_only_cache: HashMap<String, ReadOnlyCacheEntry>,
    failed_write_cache: HashMap<String, FailedWriteEntry>,
    success_write_cache: HashMap<String, SuccessWriteEntry>,
    pub last_mutation_at: u64,
    pub total_dedup_hits: u32,
    pub round_dedup_hits: u32,
    pub round_write_intercept_count: u32,
}

impl WorkerDuplicateGuard {
    pub fn new() -> Self {
        Self {
            read_only_cache: HashMap::new(),
            failed_write_cache: HashMap::new(),
            success_write_cache: HashMap::new(),
            last_mutation_at: 0,
            total_dedup_hits: 0,
            round_dedup_hits: 0,
            round_write_intercept_count: 0,
        }
    }

    pub fn clear_all(&mut self) {
        self.read_only_cache.clear();
        self.failed_write_cache.clear();
        self.success_write_cache.clear();
        self.total_dedup_hits = 0;
        self.round_dedup_hits = 0;
        self.round_write_intercept_count = 0;
        self.last_mutation_at = 0;
    }

    pub fn reset_round_counts(&mut self) {
        self.round_dedup_hits = 0;
        self.round_write_intercept_count = 0;
    }

    pub fn is_read_only_tool(&self, name: &str) -> bool {
        PERMISSION_ENGINE.is_read_only_tool(name)
    }

    pub fn is_write_dedup_tool(&self, name: &str) -> bool {
        write_dedup_tool_names().contains(name)
    }

    pub fn extract_accessed_paths(tool_calls: &[ToolCallInfo]) -> Vec<String> {
        let mut paths = Vec::new();
        for tc in tool_calls {
            if let Some(obj) = tc.arguments.as_object() {
                let path = obj
                    .get("path")
                    .or(obj.get("file"))
                    .or(obj.get("filepath"))
                    .or(obj.get("filePath"))
                    .or(obj.get("file_path"))
                    .and_then(|v| v.as_str());
                if let Some(p) = path {
                    let trimmed = p.trim();
                    if !trimmed.is_empty() {
                        paths.push(trimmed.to_string());
                        continue;
                    }
                }
                let query = obj
                    .get("query")
                    .or(obj.get("pattern"))
                    .or(obj.get("search"))
                    .and_then(|v| v.as_str());
                if let Some(q) = query {
                    let trimmed = q.trim();
                    if !trimmed.is_empty() {
                        paths.push(format!("__query:{}", trimmed));
                    }
                }
            }
        }
        paths
    }

    pub fn check_read_only_duplicate(
        &mut self,
        tool: &ToolCallInfo,
        now_ms: u64,
    ) -> Option<String> {
        if !self.is_read_only_tool(&tool.name) {
            return None;
        }
        let key = self.build_read_only_fingerprint(tool)?;
        let entry = self.read_only_cache.get_mut(&key)?;

        if now_ms.saturating_sub(entry.last_at) > READ_ONLY_DEDUP_TTL_MS {
            self.read_only_cache.remove(&key);
            return None;
        }
        if entry.last_at < self.last_mutation_at {
            self.read_only_cache.remove(&key);
            return None;
        }

        entry.count += 1;
        entry.last_at = now_ms;
        self.total_dedup_hits += 1;
        self.round_dedup_hits += 1;
        Some(format!(
            "[系统提示] 已执行过完全相同的 {} 调用（参数一致）。请直接使用已有结果推进，不要重复调用。",
            tool.name
        ))
    }

    pub fn record_read_only_call(&mut self, tool: &ToolCallInfo, now_ms: u64) {
        if !self.is_read_only_tool(&tool.name) {
            return;
        }
        if let Some(key) = self.build_read_only_fingerprint(tool) {
            if let Some(entry) = self.read_only_cache.get_mut(&key) {
                entry.last_at = now_ms;
                return;
            }
            self.read_only_cache.insert(
                key,
                ReadOnlyCacheEntry {
                    count: 1,
                    _first_at: now_ms,
                    last_at: now_ms,
                },
            );
        }
    }

    pub fn check_failed_write_duplicate(
        &mut self,
        tool: &ToolCallInfo,
        now_ms: u64,
    ) -> Option<String> {
        if !self.is_write_dedup_tool(&tool.name) {
            return None;
        }
        let key = self.build_write_operation_key(tool);
        let entry = self.failed_write_cache.get_mut(&key)?;

        if now_ms.saturating_sub(entry.last_at) > FAILED_WRITE_CACHE_TTL_MS {
            self.failed_write_cache.remove(&key);
            return None;
        }

        entry.count += 1;
        entry.last_at = now_ms;
        self.total_dedup_hits += 1;
        self.round_dedup_hits += 1;
        self.round_write_intercept_count += 1;
        Some(format!(
            "[系统拦截] 此操作已失败 {} 次，错误：{}。请勿重复相同操作，改用其他方式完成任务。",
            entry.count, entry.error,
        ))
    }

    pub fn check_success_write_duplicate(
        &mut self,
        tool: &ToolCallInfo,
        now_ms: u64,
    ) -> Option<String> {
        if !self.is_write_dedup_tool(&tool.name) {
            return None;
        }
        let key = self.build_content_aware_write_key(tool);
        let entry = self.success_write_cache.get(&key)?;

        if now_ms.saturating_sub(entry.updated_at) > SUCCESS_WRITE_CACHE_TTL_MS {
            self.success_write_cache.remove(&key);
            return None;
        }

        self.total_dedup_hits += 1;
        self.round_dedup_hits += 1;
        self.round_write_intercept_count += 1;
        Some(
            "[系统拦截] 此写操作已成功执行，结果已在上下文中。请勿重复相同操作，继续推进任务的下一步。"
                .to_string(),
        )
    }

    pub fn record_success_write(&mut self, tool: &ToolCallInfo, now_ms: u64) {
        if !self.is_write_dedup_tool(&tool.name) {
            return;
        }
        let key = self.build_content_aware_write_key(tool);
        let file_path = self.extract_write_target_path(tool).unwrap_or_default();
        self.success_write_cache.insert(
            key,
            SuccessWriteEntry {
                file_path,
                updated_at: now_ms,
            },
        );
    }

    pub fn record_failed_write(&mut self, tool: &ToolCallInfo, error: &str, now_ms: u64) {
        if !self.is_write_dedup_tool(&tool.name) {
            return;
        }
        if error.contains("[FILE_CONTEXT_STALE]") {
            return;
        }
        let key = self.build_write_operation_key(tool);
        if let Some(entry) = self.failed_write_cache.get_mut(&key) {
            entry.count += 1;
            entry.error = error.to_string();
            entry.last_at = now_ms;
        } else {
            self.failed_write_cache.insert(
                key,
                FailedWriteEntry {
                    count: 1,
                    error: error.to_string(),
                    _first_at: now_ms,
                    last_at: now_ms,
                },
            );
        }
    }

    pub fn clear_failed_write_for_path(&mut self, tool: &ToolCallInfo) {
        if !self.is_write_dedup_tool(&tool.name) {
            return;
        }
        if self.is_file_mutation_tool(&tool.name) {
            self.failed_write_cache
                .retain(|k, _| !k.starts_with("shell_exec:"));
        }
        if let Some(file_path) = self.extract_write_target_path(tool) {
            self.failed_write_cache
                .retain(|k, _| !k.contains(&file_path));
            self.success_write_cache
                .retain(|_, v| v.file_path != file_path);
        }
    }

    fn build_read_only_fingerprint(&self, tool: &ToolCallInfo) -> Option<String> {
        if !self.is_read_only_tool(&tool.name) {
            return None;
        }
        let normalized = normalize_json_value(&tool.arguments);
        Some(format!(
            "{}::{}",
            tool.name,
            serde_json::to_string(&normalized).unwrap_or_default()
        ))
    }

    fn build_write_operation_key(&self, tool: &ToolCallInfo) -> String {
        if tool.name == "shell_exec" {
            let cmd = tool
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let cwd = tool
                .arguments
                .get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            return format!("shell_exec:{}:{}", cmd, cwd);
        }
        self.build_content_aware_write_key(tool)
    }

    fn build_content_aware_write_key(&self, tool: &ToolCallInfo) -> String {
        if let Some(obj) = tool.arguments.as_object() {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let fingerprint: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}={}",
                        k,
                        serde_json::to_string(&obj[*k]).unwrap_or_default()
                    )
                })
                .collect();
            format!("{}::{}", tool.name, fingerprint.join("|"))
        } else {
            format!("{}::{}", tool.name, tool.arguments)
        }
    }

    fn extract_write_target_path(&self, tool: &ToolCallInfo) -> Option<String> {
        let obj = tool.arguments.as_object()?;
        let path = obj
            .get("path")
            .or(obj.get("file_path"))
            .or(obj.get("filePath"))
            .or(obj.get("file"))
            .and_then(|v| v.as_str())?;
        let trimmed = path.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn is_file_mutation_tool(&self, name: &str) -> bool {
        matches!(
            name,
            "file_edit" | "file_create" | "file_insert" | "file_remove"
        )
    }
}

impl Default for WorkerDuplicateGuard {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            sorted.sort_by_key(|(k, _)| *k);
            let normalized: serde_json::Map<String, serde_json::Value> = sorted
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), normalize_json_value(v)))
                .collect();
            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(normalize_json_value).collect())
        }
        serde_json::Value::String(s) => serde_json::Value::String(s.trim().to_string()),
        other => other.clone(),
    }
}
