//! MCP 客户端身份与聊天身份分离。目录只在本机批准时冻结；普通调用不改变绑定。
use std::path::PathBuf;
use std::sync::Mutex;

use magi_core::{ProjectDirectoryBinding, SessionId, UtcMillis};
use magi_mcp_server::{AttributionMode, ClientPrincipal};
use serde::{Deserialize, Serialize};

use crate::state::ApiState;

const PENDING_TTL_MS: u64 = 10 * 60 * 1000;
const MAX_PENDING: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpConversation {
    pub session_id: String,
    pub token_id: String,
    pub conversation_id: String,
    pub title: String,
    pub requested_at_ms: u64,
    pub directories: Option<ProjectDirectoryBinding>,
    #[serde(default)]
    pub denied: bool,
}

#[derive(Default)]
pub(crate) struct McpConversations {
    path: Option<PathBuf>,
    records: Mutex<Vec<McpConversation>>,
}

impl McpConversations {
    pub fn load(path: PathBuf) -> Self {
        let records = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Vec<McpConversation>>(&bytes)
                .unwrap_or_default()
                .into_iter()
                .filter(|record| record.directories.is_some() || record.denied)
                .collect(),
            Err(_) => Vec::new(),
        };
        // 待选择请求不跨 daemon 恢复，重试会重新提出；已批准会话持久恢复。
        Self {
            path: Some(path),
            records: Mutex::new(records),
        }
    }

    pub fn records(&self) -> Vec<McpConversation> {
        self.records
            .lock()
            .expect("MCP conversations poisoned")
            .clone()
    }

    fn commit(
        &self,
        records: &mut Vec<McpConversation>,
        next: Vec<McpConversation>,
    ) -> Result<(), String> {
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
            super::mcp_runtime::write_private_atomic(path, &bytes).map_err(|e| e.to_string())?;
        }
        *records = next;
        Ok(())
    }

    pub fn start(
        &self,
        principal: &ClientPrincipal,
        conversation_id: &str,
        title: &str,
    ) -> Result<McpConversation, String> {
        let id = conversation_id.trim();
        if id.is_empty() || id.len() > 256 || title.len() > 640 {
            return Err("conversation_id 必须为 1–256 字节的稳定聊天标识".into());
        }
        let mut records = self.records.lock().expect("MCP conversations poisoned");
        let now = UtcMillis::now().0;
        let mut next = records.clone();
        next.retain(|record| {
            record.directories.is_some()
                || record.denied
                || now.saturating_sub(record.requested_at_ms) < PENDING_TTL_MS
        });
        if let Some(record) = next
            .iter()
            .find(|record| record.token_id == principal.token_id && record.conversation_id == id)
        {
            return Ok(record.clone());
        }
        if next
            .iter()
            .filter(|r| r.directories.is_none() && !r.denied)
            .count()
            >= MAX_PENDING
        {
            return Err("待选择目录的会话过多，请先在 Magi 中处理".into());
        }
        let mut bytes = [0; 24];
        getrandom::fill(&mut bytes).map_err(|_| "无法生成会话身份")?;
        let random: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let record = McpConversation {
            session_id: format!("external-tool-chat-{random}"),
            token_id: principal.token_id.clone(),
            conversation_id: id.into(),
            title: title.trim().to_string(),
            requested_at_ms: now,
            directories: None,
            denied: false,
        };
        next.push(record.clone());
        self.commit(&mut records, next)?;
        Ok(record)
    }

    pub fn decide(
        &self,
        state: &ApiState,
        id: &str,
        directories: Option<ProjectDirectoryBinding>,
    ) -> Result<McpConversation, String> {
        let mut records = self.records.lock().expect("MCP conversations poisoned");
        let mut next = records.clone();
        let record = next
            .iter_mut()
            .find(|r| r.session_id == id)
            .ok_or("目录选择请求不存在")?;
        if record.directories.is_some()
            || record.denied
            || UtcMillis::now().0.saturating_sub(record.requested_at_ms) >= PENDING_TTL_MS
        {
            return Err("目录选择请求已处理或已过期".into());
        }
        if let Some(binding) = &directories {
            state
                .workspace_registry
                .validate_directory_binding(binding)
                .map_err(|e| e.to_string())?;
            let session = SessionId::new(id);
            if let Some(existing) = state.session_store.session(&session) {
                if existing.workspace_id.as_deref() != Some(binding.workspace_id.as_str()) {
                    return Err("会话归属冲突".into());
                }
            } else {
                state
                    .session_store
                    .create_external_tool_session(
                        session,
                        record.title.clone(),
                        binding.workspace_id.to_string(),
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        record.denied = directories.is_none();
        record.directories = directories;
        let result = record.clone();
        self.commit(&mut records, next)?;
        Ok(result)
    }

    pub fn bind(
        &self,
        state: &ApiState,
        principal: &ClientPrincipal,
        id: &str,
    ) -> Result<ClientPrincipal, String> {
        let record = self
            .records()
            .into_iter()
            .find(|record| record.session_id == id && record.token_id == principal.token_id)
            .ok_or("会话不属于当前 MCP 客户端，请在本聊天调用 magi.session.start")?;
        let binding = record
            .directories
            .filter(|_| !record.denied)
            .ok_or("请先在本机 Magi 选择本会话的执行目录")?;
        state
            .workspace_registry
            .validate_directory_binding(&binding)
            .map_err(|e| e.to_string())?;
        if state.session_store.session(&SessionId::new(id)).is_none() {
            return Err("外部会话已删除，请用新的 conversation_id 开始会话并重新选择目录".into());
        }
        let mut bound = principal.clone();
        bound.workspace_id = binding.workspace_id.to_string();
        bound.project_directories = Some(binding);
        bound.session_id = Some(id.into());
        Ok(bound)
    }

    pub fn pending(&self, state: &ApiState) -> Vec<McpConversation> {
        let tokens = state.mcp_service.list_tokens();
        self.records()
            .into_iter()
            .filter(|record| {
                record.directories.is_none()
                    && !record.denied
                    && UtcMillis::now().0.saturating_sub(record.requested_at_ms) < PENDING_TTL_MS
                    && tokens.iter().any(|token| {
                        token.token_id == record.token_id
                            && token.is_active(UtcMillis::now().0)
                            && token.attribution == AttributionMode::Conversation
                    })
            })
            .collect()
    }
}
