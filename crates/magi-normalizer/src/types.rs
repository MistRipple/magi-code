use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Text,
    Error,
    Interaction,
    Plan,
    ToolCall,
    Thinking,
    Progress,
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageLifecycle {
    Streaming,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    Worker,
    Orchestrator,
    Auxiliary,
    System,
    User,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageCategory {
    Content,
    Control,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionType {
    Permission,
    Clarification,
    Question,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    User,
    System,
    Debug,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextBlock),
    Thinking(ThinkingBlock),
    ToolCall(ToolCallBlock),
    Code(CodeBlock),
    FileChange(FileChangeBlock),
    Plan(PlanBlock),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextBlock {
    pub content: String,
    #[serde(default)]
    pub is_markdown: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBlock {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallBlock {
    pub tool_name: String,
    pub tool_id: String,
    pub status: ToolCallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeBlock {
    pub language: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeBlock {
    pub file_path: String,
    pub change_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBlock {
    pub content: String,
    #[serde(default)]
    pub is_markdown: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionOption {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionRequest {
    #[serde(rename = "type")]
    pub interaction_type: InteractionType,
    pub request_id: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<InteractionOption>>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardMessage {
    pub id: String,
    pub trace_id: String,
    pub category: MessageCategory,
    #[serde(rename = "type")]
    pub message_type: MessageType,
    pub source: MessageSource,
    pub agent: String,
    pub lifecycle: MessageLifecycle,
    pub timestamp: u64,
    pub updated_at: u64,
    pub blocks: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction: Option<InteractionRequest>,
    #[serde(default = "default_visibility_str")]
    pub visibility: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

fn default_visibility_str() -> String {
    "user".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "updateType")]
pub enum StreamUpdate {
    #[serde(rename = "append_text")]
    AppendText {
        message_id: String,
        timestamp: u64,
        text: String,
    },
    #[serde(rename = "merge_block")]
    MergeBlock {
        message_id: String,
        timestamp: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        blocks: Option<Vec<ContentBlock>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        token_usage: Option<TokenUsageInfo>,
    },
    #[serde(rename = "replace_blocks")]
    ReplaceBlocks {
        message_id: String,
        timestamp: u64,
        blocks: Vec<ContentBlock>,
    },
}

impl StreamUpdate {
    pub fn message_id(&self) -> &str {
        match self {
            Self::AppendText { message_id, .. } => message_id,
            Self::MergeBlock { message_id, .. } => message_id,
            Self::ReplaceBlocks { message_id, .. } => message_id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizerFamily {
    Anthropic,
    Openai,
    Google,
}

pub fn generate_message_id() -> String {
    format!("msg-{}", &uuid::Uuid::new_v4().to_string()[..8])
}
