//! 文件查找、文本搜索与范围读取共用的权限、预算和取消边界。
use super::{
    failure::{filesystem_failure, invalid_input, path_resolution_failure},
    process::ProcessExecutionScope,
    resolve_path_with_context,
};
use crate::{ToolExecutionContext, ToolExecutionContextQuery, ToolRuntimeResources};
use magi_core::ToolFailure;
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) const DEFAULT_MAX_BYTES: usize = 64 * 1024;
pub(super) const MAX_BYTES: usize = 1024 * 1024;
pub(super) const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
static NEXT_READ: AtomicU64 = AtomicU64::new(1);
type ActiveRead = (ProcessExecutionScope, Arc<AtomicBool>);
static ACTIVE_READS: LazyLock<Mutex<HashMap<u64, ActiveRead>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn cancel_reads(query: Option<&ToolExecutionContextQuery>) -> usize {
    let table = ACTIVE_READS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    table
        .values()
        .filter(|(scope, cancelled)| {
            query.is_none_or(|query| scope.matches_query(query))
                && !cancelled.swap(true, Ordering::SeqCst)
        })
        .count()
}

pub(super) fn bounded_usize(
    request: &Map<String, Value>,
    key: &str,
    default: usize,
    max: usize,
    tool: &str,
) -> Result<usize, String> {
    match request.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .map(|n| n.min(max))
            .ok_or_else(|| invalid_input(tool, format!("{key} 必须是正整数"))),
    }
}

pub(super) struct ReadOperation {
    id: u64,
    tool: &'static str,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    policy: magi_permissions::PermissionPolicy,
    profile: magi_core::AccessProfile,
}
impl Drop for ReadOperation {
    fn drop(&mut self) {
        ACTIVE_READS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}
impl ReadOperation {
    pub(super) fn new(
        tool: &'static str,
        request: &Map<String, Value>,
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
    ) -> Result<Arc<Self>, String> {
        let timeout = bounded_usize(request, "timeout_ms", 10_000, 60_000, tool)?;
        let id = NEXT_READ.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        ACTIVE_READS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                (
                    ProcessExecutionScope::from_context(context),
                    cancelled.clone(),
                ),
            );
        Ok(Arc::new(Self {
            id,
            tool,
            cancelled,
            deadline: Instant::now() + Duration::from_millis(timeout as u64),
            policy: resources.file_read_policy.clone(),
            profile: context.access_profile,
        }))
    }
    pub(super) fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(ToolFailure::new(self.tool, "cancelled", "读取已取消")
                .with("status", "cancelled")
                .into_payload());
        }
        if Instant::now() >= self.deadline {
            return Err(ToolFailure::new(self.tool, "timeout", "读取超过限定时间")
                .instruction(
                    "缩小搜索根目录、文件范围或读取范围，或增大 timeout_ms（最大 60000）。",
                )
                .into_payload());
        }
        Ok(())
    }
    pub(super) fn allows(&self, path: &Path) -> bool {
        let path = crate::canonicalize_tool_permission_path(path);
        matches!(
            crate::builtin_permission_engine().decide(
                &magi_permissions::PermissionRequest::PathAccess {
                    absolute_path: &path,
                    kind: magi_permissions::PathAccessKind::Read
                },
                &self.policy,
                self.profile
            ),
            magi_permissions::Decision::Allow
        )
    }
    pub(super) fn resolve(
        &self,
        input: &str,
        context: &ToolExecutionContext,
    ) -> Result<PathBuf, String> {
        self.check()?;
        let path = resolve_path_with_context(input, context)
            .map_err(|e| path_resolution_failure(self.tool, input, &e))?;
        self.authorize(&path)?;
        Ok(path)
    }
    pub(super) fn authorize(&self, path: &Path) -> Result<(), String> {
        self.check()?;
        if !self.allows(path) {
            return Err(
                ToolFailure::new(self.tool, "permission_denied", "策略未授权读取该路径")
                    .rejected()
                    .instruction("使用已授权的工作区路径。")
                    .into_payload(),
            );
        }
        Ok(())
    }
    pub(super) fn open(&self, path: &Path) -> Result<fs::File, String> {
        self.authorize(path)?;
        let metadata = fs::metadata(path).map_err(|e| self.io_failure(path, &e))?;
        if !metadata.is_file() {
            return Err(invalid_input(self.tool, "该路径不是普通文件"));
        }
        fs::File::open(path).map_err(|e| self.io_failure(path, &e))
    }
    pub(super) fn io_failure(&self, path: &Path, error: &io::Error) -> String {
        self.check().err().unwrap_or_else(|| {
            filesystem_failure(self.tool, "读取路径", path, error).into_payload()
        })
    }
    pub(super) fn read_chunk(
        &self,
        file: &mut fs::File,
        buffer: &mut [u8],
        path: &Path,
    ) -> Result<usize, String> {
        self.check()?;
        let n = file.read(buffer).map_err(|e| self.io_failure(path, &e))?;
        self.check()?;
        Ok(n)
    }
}

struct IgnoreLayer {
    ignore: ignore::gitignore::Gitignore,
    gitignore: ignore::gitignore::Gitignore,
}

/// 显式逐目录推进，在读取目录及 ignore 文件之前复核权限和预算。
/// ignore crate 只负责规则解析，避免遍历器内部不可取消的循环或越权读取 ignore 文件。
pub(super) struct WorkspaceFiles {
    op: Arc<ReadOperation>,
    stack: Vec<fs::ReadDir>,
    rules: Vec<IgnoreLayer>,
    single_file: Option<PathBuf>,
    include_hidden: bool,
    pub(super) unreadable: usize,
}
impl WorkspaceFiles {
    pub(super) fn new(
        op: Arc<ReadOperation>,
        root: &Path,
        context: &ToolExecutionContext,
        include_hidden: bool,
    ) -> Result<Self, String> {
        let root = crate::canonicalize_tool_permission_path(root);
        let base = context
            .working_directory
            .as_deref()
            .map(crate::canonicalize_tool_permission_path)
            .filter(|base| root.starts_with(base))
            .unwrap_or_else(|| root.clone());
        let mut walker = Self {
            op,
            stack: Vec::new(),
            rules: Vec::new(),
            single_file: None,
            include_hidden,
            unreadable: 0,
        };
        let mut ancestor = base.clone();
        // 工作区内的父级规则也适用于 root=某个子目录或文件。
        if ancestor != root {
            walker.push_rules(&ancestor)?;
            for component in root.strip_prefix(&base).unwrap().components() {
                ancestor.push(component);
                if ancestor == root {
                    break;
                }
                if walker.excluded(&ancestor, true) || !walker.op.allows(&ancestor) {
                    return Ok(walker);
                }
                walker.push_rules(&ancestor)?;
            }
        }
        let metadata = fs::metadata(&root).map_err(|e| walker.op.io_failure(&root, &e))?;
        if root != base && walker.excluded(&root, metadata.is_dir()) {
            return Ok(walker);
        }
        if metadata.is_file() {
            walker.single_file = Some(root);
        } else if metadata.is_dir() {
            walker.op.authorize(&root)?;
            walker
                .stack
                .push(fs::read_dir(&root).map_err(|e| walker.op.io_failure(&root, &e))?);
            walker.push_rules(&root)?;
        }
        Ok(walker)
    }
    fn push_rules(&mut self, dir: &Path) -> Result<(), String> {
        let ignore = self.load_rules(dir, ".ignore")?;
        let gitignore = self.load_rules(dir, ".gitignore")?;
        self.rules.push(IgnoreLayer { ignore, gitignore });
        Ok(())
    }
    fn load_rules(
        &mut self,
        dir: &Path,
        name: &str,
    ) -> Result<ignore::gitignore::Gitignore, String> {
        self.op.check()?;
        let path = dir.join(name);
        let mut builder = ignore::gitignore::GitignoreBuilder::new(dir);
        // 未授权的规则文件、符号链接和特殊文件不读取。
        if self.op.allows(&path) && fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            let mut file = self.op.open(&path)?;
            if file
                .metadata()
                .map_err(|e| self.op.io_failure(&path, &e))?
                .len()
                > MAX_FILE_BYTES as u64
            {
                return Err(invalid_input(
                    self.op.tool,
                    "ignore 文件超过 2MiB，无法可靠应用忽略规则",
                ));
            }
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 16 * 1024];
            loop {
                let n = self.op.read_chunk(&mut file, &mut buffer, &path)?;
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.len() > MAX_FILE_BYTES {
                    return Err(invalid_input(
                        self.op.tool,
                        "ignore 文件超过 2MiB，无法可靠应用忽略规则",
                    ));
                }
            }
            let content = decode_text(bytes, false)
                .map_err(|_| invalid_input(self.op.tool, "ignore 文件必须是 UTF-8 文本"))?;
            for line in content.trim_start_matches('\u{feff}').lines() {
                self.op.check()?;
                builder
                    .add_line(Some(path.clone()), line)
                    .map_err(|_| invalid_input(self.op.tool, "ignore 文件包含无效规则"))?;
            }
        }
        builder
            .build()
            .map_err(|_| invalid_input(self.op.tool, "ignore 文件包含无效规则"))
    }
    fn excluded(&self, path: &Path, is_dir: bool) -> bool {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == ".git"
            || (is_dir && matches!(name, "target" | "node_modules" | "dist" | "coverage"))
        {
            return true;
        }
        // .ignore 高于 .gitignore；同类规则以内层目录为先，保留否定模式语义。
        for is_ignore in [true, false] {
            for layer in self.rules.iter().rev() {
                let matcher = if is_ignore {
                    &layer.ignore
                } else {
                    &layer.gitignore
                };
                match matcher.matched_path_or_any_parents(path, is_dir) {
                    ignore::Match::Ignore(_) => return true,
                    ignore::Match::Whitelist(_) => return false,
                    ignore::Match::None => {}
                }
            }
        }
        !self.include_hidden && name.starts_with('.')
    }
    pub(super) fn next_file(&mut self) -> Result<Option<PathBuf>, String> {
        self.op.check()?;
        if self.single_file.is_some() {
            return Ok(self.single_file.take());
        }
        while let Some(entries) = self.stack.last_mut() {
            self.op.check()?;
            let entry = match entries.next() {
                None => {
                    self.stack.pop();
                    self.rules.pop();
                    continue;
                }
                Some(Err(_)) => {
                    self.unreadable += 1;
                    continue;
                }
                Some(Ok(entry)) => entry,
            };
            let path = entry.path();
            if !self.op.allows(&path) {
                continue;
            }
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => {
                    self.unreadable += 1;
                    continue;
                }
            };
            if kind.is_symlink() || self.excluded(&path, kind.is_dir()) {
                continue;
            }
            if kind.is_file() {
                return Ok(Some(path));
            }
            if kind.is_dir() {
                match fs::read_dir(&path) {
                    Ok(entries) => {
                        self.push_rules(&path)?;
                        self.stack.push(entries);
                    }
                    Err(_) => self.unreadable += 1,
                }
            }
        }
        self.op.check()?;
        Ok(None)
    }
}

pub(super) fn decode_text(mut bytes: Vec<u8>, truncated: bool) -> Result<String, ()> {
    if bytes.contains(&0) {
        return Err(());
    }
    match std::str::from_utf8(&bytes) {
        Ok(_) => {}
        Err(error) if truncated && error.error_len().is_none() => {
            bytes.truncate(error.valid_up_to())
        }
        Err(_) => return Err(()),
    }
    String::from_utf8(bytes).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolRegistry;

    #[test]
    fn native_read_budget_distinguishes_deadline_and_scoped_cancellation() {
        let context = ToolExecutionContext {
            session_id: Some(magi_core::SessionId::new("read-budget-test")),
            ..ToolExecutionContext::default()
        };
        let resources = ToolRuntimeResources::default();
        let mut expired =
            ReadOperation::new("search_text", &Map::new(), &context, &resources).unwrap();
        Arc::get_mut(&mut expired).unwrap().deadline = Instant::now() - Duration::from_millis(1);
        let payload: Value = serde_json::from_str(&expired.check().unwrap_err()).unwrap();
        assert_eq!(payload["error_code"], "search_text_timeout");
        assert_eq!(payload["status"], "failed");
        drop(expired);
        let operation = ReadOperation::new("file_read", &Map::new(), &context, &resources).unwrap();
        let registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        );
        assert_eq!(
            registry.cancel_active_executions(&ToolExecutionContextQuery {
                session_id: Some(magi_core::SessionId::new("other")),
                ..Default::default()
            }),
            0
        );
        assert!(operation.check().is_ok());
        let query = ToolExecutionContextQuery {
            session_id: context.session_id.clone(),
            ..Default::default()
        };
        assert_eq!(registry.cancel_active_executions(&query), 1);
        let payload: Value = serde_json::from_str(&operation.check().unwrap_err()).unwrap();
        assert_eq!(payload["status"], "cancelled");
        drop(operation);
        assert_eq!(registry.cancel_active_executions(&query), 0);
    }
    #[test]
    fn native_file_read_stops_in_flight_and_reports_timeout_through_registry() {
        let root = std::env::temp_dir().join(format!(
            "magi-read-cancel-{}-{}",
            std::process::id(),
            NEXT_READ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("large");
        fs::File::create(&path)
            .unwrap()
            .set_len(1024 * 1024 * 1024)
            .unwrap();
        let mut registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(magi_event_bus::InMemoryEventBus::new(16)),
        );
        registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(magi_core::SessionId::new(root.to_string_lossy())),
            workspace_id: Some(magi_core::WorkspaceId::new("read-cancel-workspace")),
            working_directory: Some(root.clone()),
            ..Default::default()
        };
        let query = ToolExecutionContextQuery {
            session_id: context.session_id.clone(),
            ..Default::default()
        };
        let worker_registry = registry.clone();
        let worker_context = context.clone();
        let handle = std::thread::spawn(move || {
            worker_registry.execute_with_context(
                crate::ToolExecutionInput::for_builtin_invocation(
                    magi_core::ToolCallId::new("in-flight-read"),
                    "file_read",
                    r#"{"path":"large","start_line":2}"#,
                ),
                worker_context,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if registry.cancel_active_executions(&query) == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "read did not register before deadline"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            handle.join().unwrap().status,
            magi_core::ExecutionResultStatus::Cancelled
        );
        let output = registry.execute_with_context(
            crate::ToolExecutionInput::for_builtin_invocation(
                magi_core::ToolCallId::new("deadline-read"),
                "file_read",
                r#"{"path":"large","start_line":2,"timeout_ms":1}"#,
            ),
            context,
        );
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["error_code"], "file_read_timeout");
        assert_eq!(output.status, magi_core::ExecutionResultStatus::Failed);
        fs::remove_dir_all(root).unwrap();
    }
}
