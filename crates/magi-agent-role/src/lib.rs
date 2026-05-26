//! Task System v2 — AgentRole（角色定义 + 注册表 + 文件加载）。
//!
//! role 定义以 markdown 文件承载，每个文件等价于一个 role：
//! - 头部用 `---` 包围的 YAML 风格 front-matter，承载 `id` / `supported_kinds` /
//!   `parallelism_limit` / `coordinator_mode` / `version` 五个元数据字段。
//! - body 即 system_prompt 正文，可以是多行中文/英文/Markdown，原样塞入 LLM。
//!
//! `version` 是 schema 演进锚点：当 markdown 格式发生破坏性变化（如新增必填
//! key 或重命名既有 key），loader 可凭此字段在不解析 body 的前提下识别旧
//! 文件并触发迁移。当前内置集与默认值均为 `1`。
//!
//! 加载顺序：
//! 1. crate 内置 builtin 集（编译期 `include_str!` 嵌入，5 个代理角色 + 1 个内部主线协调器）
//! 2. 用户 override（`~/.magi/roles/*.md`），同 id 时 user override 覆盖 builtin
//!
//! 解析失败（front-matter 缺失、字段无法识别）走 tracing warn，跳过该文件而不
//! 阻塞 daemon 启动——builtin 集解析失败因为是编译期常量，会在测试阶段就发现。
//!
//! 格式与 `~/.claude/agents/*.md` 同源（YAML front-matter + markdown body），方便
//! 手写、git diff 与人眼 review；不再使用 JSON，让所有角色文件形态保持唯一。

use magi_core::TaskKind;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, thiserror::Error)]
pub enum AgentRoleError {
    #[error("read role file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse role file {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("role file {path} 缺少 id（文件名 {file_stem} 无效）")]
    InvalidId { path: PathBuf, file_stem: String },
}

/// 单个 role 的定义。
///
/// `coordinator_mode = true` 表示该角色采用 Prompt-as-Code 主线编排模式：
/// LLM 通过 `agent_spawn` 创建代理并投递任务消息，随后通过 `agent_wait`
/// 收集代理终态结果；整个 orchestration 由 prompt 驱动，而不是
/// Code-as-Coordinator 在外层硬编码状态机。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRole {
    #[serde(default)]
    pub id: String,
    pub system_prompt: String,
    #[serde(default)]
    pub supported_kinds: Vec<TaskKindLabel>,
    #[serde(default)]
    pub parallelism_limit: Option<u32>,
    #[serde(default)]
    pub coordinator_mode: bool,
    /// Schema 演进锚点。当 markdown front-matter 出现破坏性变化时，loader 可凭
    /// 此字段识别版本并触发迁移。默认值 = 1，对应当前 schema。
    #[serde(default = "default_role_version")]
    pub version: u32,
}

fn default_role_version() -> u32 {
    1
}

/// 用字符串标签序列化 TaskKind，便于人手写 markdown front-matter。
/// 标签语义与 magi-core 的 TaskKind 保持一一对应。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKindLabel {
    LocalAgent,
    LocalWorkflow,
    RemoteAgent,
    MonitorMcp,
    InProcessTeammate,
    Dream,
}

impl TaskKindLabel {
    pub fn to_task_kind(self) -> TaskKind {
        match self {
            Self::LocalAgent => TaskKind::LocalAgent,
            Self::LocalWorkflow => TaskKind::LocalWorkflow,
            Self::RemoteAgent => TaskKind::RemoteAgent,
            Self::MonitorMcp => TaskKind::MonitorMcp,
            Self::InProcessTeammate => TaskKind::InProcessTeammate,
            Self::Dream => TaskKind::Dream,
        }
    }

    fn parse(label: &str) -> Option<Self> {
        match label {
            "local_agent" => Some(Self::LocalAgent),
            "local_workflow" => Some(Self::LocalWorkflow),
            "remote_agent" => Some(Self::RemoteAgent),
            "monitor_mcp" => Some(Self::MonitorMcp),
            "in_process_teammate" => Some(Self::InProcessTeammate),
            "dream" => Some(Self::Dream),
            _ => None,
        }
    }
}

impl AgentRole {
    pub fn supported_task_kinds(&self) -> Vec<TaskKind> {
        self.supported_kinds
            .iter()
            .map(|k| k.to_task_kind())
            .collect()
    }
}

/// 进程内角色注册表。注册表本身只读不可变；运行时若要刷新（reload），重新
/// 构造一个新的 Arc 并替换即可。
#[derive(Clone, Debug, Default)]
pub struct AgentRoleRegistry {
    roles: Arc<HashMap<String, AgentRole>>,
}

impl AgentRoleRegistry {
    pub fn empty() -> Self {
        Self {
            roles: Arc::new(HashMap::new()),
        }
    }

    pub fn from_map(map: HashMap<String, AgentRole>) -> Self {
        Self {
            roles: Arc::new(map),
        }
    }

    /// 默认加载入口：先 builtin defaults，再叠加 user override (`~/.magi/roles/*.md`)。
    /// 用户路径不存在或读取失败不报错，自动回落到 builtin 集——这是 daemon 启动
    /// 路径，绝不阻塞。
    pub fn load_default() -> Self {
        let mut map = builtin_roles_map();
        if let Some(dir) = user_role_dir() {
            if dir.exists() {
                match load_dir(&dir) {
                    Ok(user_roles) => {
                        for role in user_roles {
                            map.insert(role.id.clone(), role);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(?err, dir = %dir.display(), "加载 ~/.magi/roles 失败，使用 builtin 默认集");
                    }
                }
            }
        }
        Self::from_map(map)
    }

    pub fn get(&self, role_id: &str) -> Option<&AgentRole> {
        self.roles.get(role_id)
    }

    pub fn contains(&self, role_id: &str) -> bool {
        self.roles.contains_key(role_id)
    }

    pub fn supported_task_kinds(&self, role_id: &str) -> Vec<TaskKind> {
        self.roles
            .get(role_id)
            .map(|r| r.supported_task_kinds())
            .unwrap_or_default()
    }

    pub fn role_supports_task_kind(&self, role_id: &str, kind: TaskKind) -> bool {
        self.supported_task_kinds(role_id).contains(&kind)
    }

    /// 判断 role 是否允许作为 agent_spawn 的目标。
    ///
    /// coordinator_mode 角色是主线编排身份，只能由 root task 使用；agent_spawn 只能派发
    /// 非 coordinator 的专业代理，避免出现“协调器派生协调器”的递归编排语义。
    pub fn is_spawnable_agent_role(&self, role_id: &str) -> bool {
        self.roles.get(role_id).is_some_and(|role| {
            !role.coordinator_mode && role.supported_task_kinds().contains(&TaskKind::LocalAgent)
        })
    }

    pub fn spawnable_agent_role_ids(&self) -> Vec<String> {
        let mut ids = self
            .roles
            .values()
            .filter(|role| self.is_spawnable_agent_role(&role.id))
            .map(|role| role.id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn all(&self) -> impl Iterator<Item = &AgentRole> {
        self.roles.values()
    }

    pub fn role_ids(&self) -> Vec<String> {
        self.roles.keys().cloned().collect()
    }

    /// 返回首个 `coordinator_mode = true` 的角色。多个协调器角色并存时仍然有效——
    /// 调用方按 role_id 显式选用；本方法仅用于在没有显式选择时拿到默认协调器。
    pub fn default_coordinator(&self) -> Option<&AgentRole> {
        self.roles.values().find(|role| role.coordinator_mode)
    }
}

fn user_role_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".magi").join("roles"))
}

fn load_dir(dir: &Path) -> Result<Vec<AgentRole>, AgentRoleError> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| AgentRoleError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        match load_file(&path) {
            Ok(role) => out.push(role),
            Err(err) => {
                tracing::warn!(?err, file = %path.display(), "跳过 role 文件");
            }
        }
    }
    Ok(out)
}

fn load_file(path: &Path) -> Result<AgentRole, AgentRoleError> {
    let raw = fs::read_to_string(path).map_err(|source| AgentRoleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut role = parse_role_markdown(&raw).map_err(|message| AgentRoleError::Parse {
        path: path.to_path_buf(),
        message,
    })?;
    if role.id.trim().is_empty() {
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if file_stem.is_empty() {
            return Err(AgentRoleError::InvalidId {
                path: path.to_path_buf(),
                file_stem,
            });
        }
        role.id = file_stem;
    }
    Ok(role)
}

/// builtin 默认角色集。用户在 `~/.magi/roles/<id>.md` 提供同名文件即可覆盖。
///
/// 编译期通过 `include_str!` 嵌入每一个角色 .md；新增 builtin 角色必须同时
/// 在 `BUILTIN_ROLE_SOURCES` 数组里挂上一行——这里有意写成静态数组（而不是
/// build.rs / 运行时扫盘），让"哪些角色是 builtin"在源代码里 grep 即得。
fn builtin_roles_map() -> HashMap<String, AgentRole> {
    let mut map = HashMap::new();
    for (label, raw) in BUILTIN_ROLE_SOURCES {
        let role = parse_role_markdown(raw).unwrap_or_else(|err| {
            panic!("builtin role {label} 解析失败（编译期资产，应在测试阶段拦截）: {err}");
        });
        map.insert(role.id.clone(), role);
    }
    map
}

const BUILTIN_ROLE_SOURCES: &[(&str, &str)] = &[
    (
        "architect",
        include_str!("../assets/builtin-roles/architect.md"),
    ),
    (
        "executor",
        include_str!("../assets/builtin-roles/executor.md"),
    ),
    (
        "explorer",
        include_str!("../assets/builtin-roles/explorer.md"),
    ),
    (
        "reviewer",
        include_str!("../assets/builtin-roles/reviewer.md"),
    ),
    ("tester", include_str!("../assets/builtin-roles/tester.md")),
    (
        "coordinator",
        include_str!("../assets/builtin-roles/coordinator.md"),
    ),
];

/// 把 `---` 包围的 front-matter + markdown body 解析为 `AgentRole`。
///
/// front-matter 支持的字段（flat YAML，不递归）：
/// - `id: <string>`
/// - `supported_kinds: [a, b]`（中括号 + 逗号分隔，元素是 snake_case 标签）
/// - `parallelism_limit: <u32>`
/// - `coordinator_mode: true|false`
/// - `version: <u32>`（缺省 = 1）
///
/// 选择"手写 mini parser"而不是引 serde_yaml 的理由：
/// 1. front-matter 字段集合是封闭已知的 5 个 key，没有递归结构；
/// 2. 引一个完整 YAML 解析器（serde_yaml 链路依赖 unmaintained 警告）只为这 5 个
///    字段，体积/编译时间不划算；
/// 3. 解析逻辑就放在本 crate 里，错误信息能直接指出"第几行哪个 key 错了"。
fn parse_role_markdown(raw: &str) -> Result<AgentRole, String> {
    let trimmed = raw.trim_start_matches('\u{feff}');
    let trimmed = trimmed.trim_start();
    let after_open = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
        .ok_or_else(|| "缺少起始 `---` 行".to_string())?;
    let close_idx =
        find_close_delimiter(after_open).ok_or_else(|| "缺少结束 `---` 行".to_string())?;
    let header = &after_open[..close_idx.start];
    let body = after_open[close_idx.end..].trim_start_matches(['\n', '\r']);

    let mut id: String = String::new();
    let mut supported_kinds: Vec<TaskKindLabel> = Vec::new();
    let mut parallelism_limit: Option<u32> = None;
    let mut coordinator_mode = false;
    let mut version: u32 = default_role_version();

    for (lineno, line) in header.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = trimmed
            .split_once(':')
            .ok_or_else(|| format!("第 {} 行不是 key: value 形式: {trimmed}", lineno + 1))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "id" => id = strip_inline_quotes(value).to_string(),
            "supported_kinds" => {
                supported_kinds = parse_kind_list(value, lineno + 1)?;
            }
            "parallelism_limit" => {
                let n: u32 = value.parse().map_err(|err| {
                    format!("第 {} 行 parallelism_limit 不是整数: {err}", lineno + 1)
                })?;
                parallelism_limit = Some(n);
            }
            "coordinator_mode" => match value {
                "true" => coordinator_mode = true,
                "false" => coordinator_mode = false,
                other => {
                    return Err(format!(
                        "第 {} 行 coordinator_mode 仅支持 true/false，收到 {other}",
                        lineno + 1
                    ));
                }
            },
            "version" => {
                version = value
                    .parse()
                    .map_err(|err| format!("第 {} 行 version 不是整数: {err}", lineno + 1))?;
            }
            other => {
                return Err(format!("第 {} 行未识别字段 `{other}`", lineno + 1));
            }
        }
    }

    let system_prompt = body.trim_end().to_string();
    if system_prompt.is_empty() {
        return Err("body 为空：缺少 system_prompt 正文".to_string());
    }

    Ok(AgentRole {
        id,
        system_prompt,
        supported_kinds,
        parallelism_limit,
        coordinator_mode,
        version,
    })
}

struct DelimRange {
    start: usize,
    end: usize,
}

fn find_close_delimiter(after_open: &str) -> Option<DelimRange> {
    // 在 header 区段里寻找单独成行的 `---`，兼容 LF / CRLF。
    let mut search_from = 0usize;
    while let Some(rel) = after_open[search_from..].find("---") {
        let abs = search_from + rel;
        let starts_at_line = abs == 0 || matches!(after_open.as_bytes()[abs - 1], b'\n');
        if starts_at_line {
            let after = abs + 3;
            let bytes = after_open.as_bytes();
            let line_end_ok = match bytes.get(after) {
                None => true,
                Some(b'\n') => true,
                Some(b'\r') if matches!(bytes.get(after + 1), Some(b'\n')) => true,
                _ => false,
            };
            if line_end_ok {
                let end_consumed = match bytes.get(after) {
                    Some(b'\n') => after + 1,
                    Some(b'\r') if bytes.get(after + 1) == Some(&b'\n') => after + 2,
                    _ => after,
                };
                return Some(DelimRange {
                    start: abs,
                    end: end_consumed,
                });
            }
        }
        search_from = abs + 3;
    }
    None
}

fn strip_inline_quotes(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn parse_kind_list(value: &str, lineno: usize) -> Result<Vec<TaskKindLabel>, String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .ok_or_else(|| format!("第 {lineno} 行 supported_kinds 期望 [a, b] 格式，收到 {value}"))?;
    let mut out = Vec::new();
    for part in inner.split(',') {
        let p = strip_inline_quotes(part.trim());
        if p.is_empty() {
            continue;
        }
        let label = TaskKindLabel::parse(p)
            .ok_or_else(|| format!("第 {lineno} 行 supported_kinds 含未识别标签 `{p}`"))?;
        out.push(label);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_set_covers_core_roles() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for role in [
            "architect",
            "executor",
            "reviewer",
            "explorer",
            "tester",
            "coordinator",
        ] {
            assert!(reg.get(role).is_some(), "missing builtin role: {role}");
        }
    }

    #[test]
    fn architect_supports_local_agent() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("architect").expect("architect role exists");
        let kinds = role.supported_task_kinds();
        assert!(kinds.contains(&TaskKind::LocalAgent));
    }

    #[test]
    fn architect_prompt_body_keeps_user_advocate_clause() {
        // body 是直接抽出来的 markdown 正文，最关键的 invariant 是它仍然包含
        // “唯一代言人” 这条规则——这条出错说明 front-matter 解析吞掉了正文。
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("architect").expect("architect role exists");
        assert!(
            role.system_prompt.contains("唯一代言人"),
            "architect prompt 必须保留 唯一代言人 条款，实际: {}",
            role.system_prompt
        );
    }

    #[test]
    fn role_supports_task_kind_handles_missing_role() {
        let reg = AgentRoleRegistry::empty();
        assert!(!reg.role_supports_task_kind("missing", TaskKind::LocalAgent));
    }

    #[test]
    fn builtin_set_contains_coordinator_with_coordinator_mode() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("coordinator").expect("coordinator role exists");
        assert!(
            role.coordinator_mode,
            "coordinator role must enable coordinator_mode"
        );
        let default = reg
            .default_coordinator()
            .expect("default_coordinator resolves");
        assert_eq!(default.id, "coordinator");
    }

    #[test]
    fn non_coordinator_roles_have_coordinator_mode_false_by_default() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for id in ["architect", "executor", "reviewer", "explorer"] {
            let role = reg.get(id).expect("builtin role exists");
            assert!(
                !role.coordinator_mode,
                "{id} should NOT default to coordinator_mode",
            );
        }
    }

    #[test]
    fn parse_role_markdown_handles_all_fields() {
        let raw = "---\nid: ml-engineer\nsupported_kinds: [local_agent, local_workflow]\nparallelism_limit: 2\ncoordinator_mode: false\nversion: 1\n---\n你是机器学习工程师\n";
        let role = parse_role_markdown(raw).expect("解析成功");
        assert_eq!(role.id, "ml-engineer");
        assert_eq!(role.system_prompt, "你是机器学习工程师");
        assert_eq!(role.parallelism_limit, Some(2));
        assert_eq!(role.supported_task_kinds().len(), 2);
        assert!(!role.coordinator_mode);
        assert_eq!(role.version, 1);
    }

    #[test]
    fn parse_role_markdown_defaults_version_to_one() {
        // 缺省 version 字段时回落到 1，保证既有 builtin 文件不需要全量加 version 也可解析。
        let raw = "---\nid: legacy\nsupported_kinds: [local_agent]\n---\n你是 legacy\n";
        let role = parse_role_markdown(raw).expect("解析成功");
        assert_eq!(role.version, 1);
    }

    #[test]
    fn parse_role_markdown_rejects_non_integer_version() {
        let err = parse_role_markdown(
            "---\nid: foo\nsupported_kinds: [local_agent]\nversion: v1\n---\n你是 foo\n",
        )
        .expect_err("version 非整数应失败");
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn builtin_roles_all_at_version_one() {
        // 守护内置集与代码默认版本同步——schema 升版时必须同步改这里。
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for role in reg.all() {
            assert_eq!(
                role.version, 1,
                "builtin role {} version 应为 1，实际 {}",
                role.id, role.version
            );
        }
    }

    #[test]
    fn parse_role_markdown_rejects_missing_front_matter() {
        let err = parse_role_markdown("你是 X").expect_err("缺 front-matter 应失败");
        assert!(err.contains("起始"), "{err}");
    }

    #[test]
    fn parse_role_markdown_rejects_unclosed_front_matter() {
        let err = parse_role_markdown("---\nid: foo\n你是 X\n").expect_err("缺尾部 --- 应失败");
        assert!(err.contains("结束"), "{err}");
    }

    #[test]
    fn parse_role_markdown_rejects_empty_body() {
        let err = parse_role_markdown("---\nid: foo\n---\n").expect_err("空 body 应失败");
        assert!(err.contains("body"), "{err}");
    }

    #[test]
    fn parse_role_markdown_rejects_unknown_kind() {
        let err =
            parse_role_markdown("---\nid: foo\nsupported_kinds: [magic_thing]\n---\n你是 foo\n")
                .expect_err("未识别 kind 应失败");
        assert!(err.contains("magic_thing"), "{err}");
    }

    #[test]
    fn load_file_parses_markdown() {
        let dir = tempdir();
        let path = dir.join("ml-engineer.md");
        fs::write(
            &path,
            "---\nid: ml-engineer\nsupported_kinds: [local_agent, local_workflow]\nparallelism_limit: 2\n---\n你是机器学习工程师\n",
        )
        .unwrap();
        let role = load_file(&path).unwrap();
        assert_eq!(role.id, "ml-engineer");
        assert_eq!(role.parallelism_limit, Some(2));
        assert_eq!(role.supported_task_kinds().len(), 2);
    }

    #[test]
    fn load_file_infers_id_from_filename() {
        let dir = tempdir();
        let path = dir.join("custom.md");
        fs::write(
            &path,
            "---\nid: \"\"\nsupported_kinds: [local_agent]\n---\n你是 custom\n",
        )
        .unwrap();
        let role = load_file(&path).unwrap();
        assert_eq!(role.id, "custom");
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "magi-agent-role-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }
}
