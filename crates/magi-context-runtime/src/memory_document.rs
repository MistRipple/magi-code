use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryContent {
    pub session_id: String,
    pub session_name: String,
    pub last_updated: String,
    pub token_estimate: usize,
    #[serde(default)]
    pub tasks: Vec<TaskRecord>,
    #[serde(default)]
    pub decisions: Vec<Decision>,
    #[serde(default)]
    pub code_changes: Vec<CodeChange>,
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub resolved_issues: Vec<ResolvedIssue>,
    #[serde(default)]
    pub rejected_approaches: Vec<RejectedApproach>,
    #[serde(default)]
    pub user_messages: Vec<UserMessage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub task_id: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub description: String,
    pub rationale: String,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeChange {
    pub file: String,
    pub change_type: String,
    pub description: String,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub description: String,
    pub severity: String,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedIssue {
    pub description: String,
    pub resolution: String,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedApproach {
    pub description: String,
    pub reason: String,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub content: String,
    pub timestamp: String,
}

impl MemoryContent {
    pub fn new(session_id: &str, session_name: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            last_updated: chrono_now_iso(),
            token_estimate: 0,
            tasks: Vec::new(),
            decisions: Vec::new(),
            code_changes: Vec::new(),
            issues: Vec::new(),
            resolved_issues: Vec::new(),
            rejected_approaches: Vec::new(),
            user_messages: Vec::new(),
        }
    }
}

pub struct MemoryDocument {
    session_id: String,
    file_path: PathBuf,
    content: MemoryContent,
    dirty: bool,
}

impl MemoryDocument {
    pub fn new(session_id: &str, session_name: &str, storage_path: &Path) -> Self {
        let file_path = storage_path.join(session_id).join("memory.json");
        Self {
            session_id: session_id.to_string(),
            file_path,
            content: MemoryContent::new(session_id, session_name),
            dirty: false,
        }
    }

    pub fn load(&mut self) -> Result<(), String> {
        if !self.file_path.exists() {
            self.save()?;
            return Ok(());
        }
        let data = fs::read_to_string(&self.file_path)
            .map_err(|e| format!("读取 memory 文件失败: {e}"))?;
        self.content =
            serde_json::from_str(&data).map_err(|e| format!("解析 memory 文件失败: {e}"))?;
        Ok(())
    }

    pub fn save(&mut self) -> Result<(), String> {
        let dir = self
            .file_path
            .parent()
            .ok_or_else(|| "无效路径".to_string())?;
        if !dir.exists() {
            fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
        }

        self.content.last_updated = chrono_now_iso();
        self.content.token_estimate = self.estimate_tokens();

        let payload =
            serde_json::to_string_pretty(&self.content).map_err(|e| format!("序列化失败: {e}"))?;

        let tmp_path = dir.join(format!(
            ".memory-{}-{}.tmp",
            self.session_id,
            std::process::id()
        ));
        fs::write(&tmp_path, &payload).map_err(|e| format!("写入临时文件失败: {e}"))?;
        fs::rename(&tmp_path, &self.file_path).map_err(|e| format!("原子重命名失败: {e}"))?;

        self.dirty = false;
        Ok(())
    }

    pub fn content(&self) -> &MemoryContent {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut MemoryContent {
        self.dirty = true;
        &mut self.content
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn add_task(&mut self, record: TaskRecord) {
        self.content.tasks.push(record);
        self.dirty = true;
    }

    pub fn add_decision(&mut self, decision: Decision) {
        self.content.decisions.push(decision);
        self.dirty = true;
    }

    pub fn add_code_change(&mut self, change: CodeChange) {
        self.content.code_changes.push(change);
        self.dirty = true;
    }

    pub fn add_issue(&mut self, issue: Issue) {
        self.content.issues.push(issue);
        self.dirty = true;
    }

    pub fn resolve_issue(&mut self, resolved: ResolvedIssue) {
        self.content.resolved_issues.push(resolved);
        self.dirty = true;
    }

    pub fn add_rejected_approach(&mut self, rejected: RejectedApproach) {
        self.content.rejected_approaches.push(rejected);
        self.dirty = true;
    }

    pub fn add_user_message(&mut self, message: UserMessage) {
        self.content.user_messages.push(message);
        self.dirty = true;
    }

    pub fn render_markdown(&self) -> String {
        let c = &self.content;
        let mut parts = Vec::new();

        parts.push(format!(
            "# 会话记忆: {}\n会话ID: {}\n更新: {}",
            c.session_name, c.session_id, c.last_updated
        ));

        if !c.tasks.is_empty() {
            let mut section = String::from("\n## 任务记录\n");
            for t in &c.tasks {
                section.push_str(&format!(
                    "- [{}] {} ({})\n",
                    t.status,
                    t.title,
                    t.summary.as_deref().unwrap_or("")
                ));
            }
            parts.push(section);
        }

        if !c.decisions.is_empty() {
            let mut section = String::from("\n## 决策记录\n");
            for d in &c.decisions {
                section.push_str(&format!("- {}: {}\n", d.description, d.rationale));
            }
            parts.push(section);
        }

        if !c.issues.is_empty() {
            let mut section = String::from("\n## 待解决问题\n");
            for i in &c.issues {
                section.push_str(&format!("- [{}] {}\n", i.severity, i.description));
            }
            parts.push(section);
        }

        parts.join("\n")
    }

    fn estimate_tokens(&self) -> usize {
        let json = serde_json::to_string(&self.content).unwrap_or_default();
        json.len() / 4 + 1
    }
}

fn chrono_now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    format!("{secs}")
}
