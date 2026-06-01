use magi_core::{
    AccessProfile, ApprovalRequirement, EventId, ExecutionResultStatus, RiskLevel, SessionId,
    TaskId, ToolCallId, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::{
    DecisionPhase, GovernanceDecision, GovernanceOutcome, GovernanceService, ToolExecutionRequest,
    ToolKind,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, RwLock},
};

mod apply_patch;
mod builtin;
mod policy;
mod tool_catalog;
mod view_image;
pub use apply_patch::apply_patch_declared_paths_from_input;
use builtin::{NormalizedBuiltinTool, infer_execution_status};
use policy::WriteProtectionClaim;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinToolName {
    // ── 文件系统 ──
    FileRead,
    ViewImage,
    FileWrite,
    FilePatch,
    ApplyPatch,
    FileRemove,
    FileMkdir,
    FileCopy,
    FileMove,
    // ── 搜索 ──
    SearchText,
    SearchSemantic,
    // ── Shell / 进程 ──
    ShellExec,
    ProcessLaunch,
    ProcessRead,
    ProcessWrite,
    ProcessKill,
    ProcessList,
    ProcessInspect,
    // ── Diff ──
    DiffPreview,
    // ── Web ──
    WebSearch,
    WebFetch,
    // ── 可视化 ──
    DiagramRender,
    // ── 知识库 ──
    KnowledgeQuery,
    // ── 代码符号导航（基于本地索引引擎的符号表）──
    /// 符号导航：按符号名查定义、或列出某文件的全部符号。
    CodeSymbols,
    // ── 工具目录 / 健康诊断 ──
    /// 列出当前内置工具目录、访问模式、风险等级与 schema 健康状态。
    ToolCatalog,
    // ── 协调器（任务系统 L10，仅 coordinator_mode 角色可见）──
    /// 派发新的代理执行子任务。该工具只创建代理并投递初始任务消息；
    /// 后续由 agent_wait 收集代理终态结果。
    AgentSpawn,
    /// 等待一个或多个已派发代理进入终态，并把代理最终答复返回给主线。
    AgentWait,
    // ── In-session 思维锚点（任务系统 L13）──
    /// 写入本 session 的 TodoLedger。整体替换列表语义（参考 claude-code 的 TodoWrite）。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    TodoWrite,
    // ── 跨 session 项目记忆（任务系统 L14）──
    /// 写入或删除当前 workspace 的 ProjectMemory entry。物理存储在
    /// `~/.magi/projects/{slug}/memory/`，跨 conversation 自动加载到 system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    MemoryWrite,
    // ── Mission 宪章（任务系统 Tier 4 / L15）──
    /// 增量写入当前 mission 的 charter（title / goal / success_criteria /
    /// constraints / stakeholders）。物理存储在
    /// `~/.magi/projects/{slug}/missions/{mission_id}/charter.md`。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    MissionCharterWrite,
    // ── Mission 执行计划（任务系统 Tier 4 / L16）──
    /// 整体替换当前 mission 的 plan.steps（id / content / status / depends_on / notes）。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/plan.md`，
    /// 每次 Turn 起始把当前 plan 自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    PlanWrite,
    // ── Mission KnowledgeGraph（任务系统 Tier 4 / L18）──
    /// 按 (kind, id) upsert 当前 mission 的 KnowledgeGraph 事实（symbol / decision / risk）。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/knowledge.md`，
    /// 每次 Turn 起始把当前 KG 自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    KgWrite,
    // ── Mission ValidationRunner（任务系统 Tier 4 / L19）──
    /// 把单条验证结果（test_suite / type_check / integration_smoke / benchmark）按
    /// (plan_step_id, kind) upsert 进当前 mission 的 ValidationReport。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/validation.md`，
    /// 每次 Turn 起始把当前 Validation 现状自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    ValidationRecord,
    // ── Mission Checkpoint（任务系统 Tier 4 / L20）──
    /// Append-only 写入一条 mission 级检查点（process_restart / context_compaction /
    /// phase_transition / manual），用于事后恢复 mission 状态。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/checkpoints.md`，
    /// 每次 Turn 起始把最新若干检查点自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    Checkpoint,
    // ── Mission HumanCheckpoint（任务系统 Tier 4 / L21）──
    /// 由 orchestrator 申请的人工审核点，在 operator 给出 approve / reject 之前
    /// mission 会进入 awaiting_human 状态。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/human_checkpoints.md`，
    /// 每次 Turn 起始把当前待解决与最近若干已解决记录注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    HumanCheckpointRequest,
}

impl BuiltinToolName {
    pub const ALL: [Self; 35] = [
        Self::FileRead,
        Self::ViewImage,
        Self::FileWrite,
        Self::FilePatch,
        Self::ApplyPatch,
        Self::FileRemove,
        Self::FileMkdir,
        Self::FileCopy,
        Self::FileMove,
        Self::SearchText,
        Self::SearchSemantic,
        Self::ShellExec,
        Self::ProcessLaunch,
        Self::ProcessRead,
        Self::ProcessWrite,
        Self::ProcessKill,
        Self::ProcessList,
        Self::ProcessInspect,
        Self::DiffPreview,
        Self::WebSearch,
        Self::WebFetch,
        Self::DiagramRender,
        Self::KnowledgeQuery,
        Self::CodeSymbols,
        Self::ToolCatalog,
        Self::AgentSpawn,
        Self::AgentWait,
        Self::TodoWrite,
        Self::MemoryWrite,
        Self::MissionCharterWrite,
        Self::PlanWrite,
        Self::KgWrite,
        Self::ValidationRecord,
        Self::Checkpoint,
        Self::HumanCheckpointRequest,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FileRead => "file_read",
            Self::ViewImage => "view_image",
            Self::FileWrite => "file_write",
            Self::FilePatch => "file_patch",
            Self::ApplyPatch => "apply_patch",
            Self::FileRemove => "file_remove",
            Self::FileMkdir => "file_mkdir",
            Self::FileCopy => "file_copy",
            Self::FileMove => "file_move",
            Self::SearchText => "search_text",
            Self::SearchSemantic => "search_semantic",
            Self::ShellExec => "shell_exec",
            Self::ProcessLaunch => "process_launch",
            Self::ProcessRead => "process_read",
            Self::ProcessWrite => "process_write",
            Self::ProcessKill => "process_kill",
            Self::ProcessList => "process_list",
            Self::ProcessInspect => "process_inspect",
            Self::DiffPreview => "diff_preview",
            Self::WebSearch => "web_search",
            Self::WebFetch => "web_fetch",
            Self::DiagramRender => "diagram_render",
            Self::KnowledgeQuery => "knowledge_query",
            Self::CodeSymbols => "code_symbols",
            Self::ToolCatalog => "tool_catalog",
            Self::AgentSpawn => "agent_spawn",
            Self::AgentWait => "agent_wait",
            Self::TodoWrite => "todo_write",
            Self::MemoryWrite => "memory_write",
            Self::MissionCharterWrite => "mission_charter_write",
            Self::PlanWrite => "plan_write",
            Self::KgWrite => "kg_write",
            Self::ValidationRecord => "validation_record",
            Self::Checkpoint => "checkpoint_create",
            Self::HumanCheckpointRequest => "human_checkpoint_request",
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            Self::FileRead
            | Self::ViewImage
            | Self::FileWrite
            | Self::FilePatch
            | Self::ApplyPatch
            | Self::FileRemove
            | Self::FileMkdir
            | Self::FileCopy
            | Self::FileMove => "filesystem",
            Self::SearchText | Self::SearchSemantic | Self::CodeSymbols => "code_navigation",
            Self::ShellExec
            | Self::ProcessLaunch
            | Self::ProcessRead
            | Self::ProcessWrite
            | Self::ProcessKill
            | Self::ProcessList
            | Self::ProcessInspect => "process",
            Self::DiffPreview => "diff",
            Self::WebSearch | Self::WebFetch => "web",
            Self::DiagramRender => "visualization",
            Self::KnowledgeQuery => "knowledge",
            Self::ToolCatalog => "tooling",
            Self::AgentSpawn | Self::AgentWait => "agent_coordination",
            Self::TodoWrite => "session_state",
            Self::MemoryWrite => "project_memory",
            Self::MissionCharterWrite
            | Self::PlanWrite
            | Self::KgWrite
            | Self::ValidationRecord
            | Self::Checkpoint
            | Self::HumanCheckpointRequest => "mission_governance",
        }
    }

    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "file_read" | "file_view" => Some(Self::FileRead),
            "view_image" | "image_view" => Some(Self::ViewImage),
            "file_write" | "file_create" => Some(Self::FileWrite),
            "file_patch" | "file_edit" | "file_insert" => Some(Self::FilePatch),
            "apply_patch" => Some(Self::ApplyPatch),
            "file_remove" => Some(Self::FileRemove),
            "file_mkdir" => Some(Self::FileMkdir),
            "file_copy" => Some(Self::FileCopy),
            "file_move" => Some(Self::FileMove),
            "search_text" | "code_search_regex" => Some(Self::SearchText),
            "search_semantic" | "code_search_semantic" => Some(Self::SearchSemantic),
            "shell_exec" => Some(Self::ShellExec),
            "process_launch" => Some(Self::ProcessLaunch),
            "process_read" => Some(Self::ProcessRead),
            "process_write" => Some(Self::ProcessWrite),
            "process_kill" => Some(Self::ProcessKill),
            "process_list" => Some(Self::ProcessList),
            "process_inspect" => Some(Self::ProcessInspect),
            "diff_preview" => Some(Self::DiffPreview),
            "web_search" => Some(Self::WebSearch),
            "web_fetch" => Some(Self::WebFetch),
            "diagram_render" => Some(Self::DiagramRender),
            "knowledge_query" | "project_knowledge_query" => Some(Self::KnowledgeQuery),
            "code_symbols" | "symbol_nav" | "goto_definition" | "list_file_symbols" => {
                Some(Self::CodeSymbols)
            }
            "tool_catalog" | "tool_diagnostics" | "builtin_tools" | "builtin_tool_catalog" => {
                Some(Self::ToolCatalog)
            }
            "agent_spawn" | "agent" | "spawn_agent" => Some(Self::AgentSpawn),
            "agent_wait" | "wait_agent" => Some(Self::AgentWait),
            "todo_write" | "todowrite" | "todo" => Some(Self::TodoWrite),
            "memory_write" | "memorywrite" | "memory" => Some(Self::MemoryWrite),
            "mission_charter_write" | "missioncharterwrite" | "mission_charter" => {
                Some(Self::MissionCharterWrite)
            }
            "plan_write" | "planwrite" | "plan" => Some(Self::PlanWrite),
            "kg_write" | "kgwrite" | "knowledge_write" | "knowledge_graph_write" => {
                Some(Self::KgWrite)
            }
            "validation_record" | "validationrecord" | "validation_write" | "validation" => {
                Some(Self::ValidationRecord)
            }
            "checkpoint_create" | "checkpoint" | "snapshot" => Some(Self::Checkpoint),
            "human_checkpoint_request" | "human_checkpoint" | "human_review" => {
                Some(Self::HumanCheckpointRequest)
            }
            _ => None,
        }
    }

    pub fn is_write_operation(&self) -> bool {
        matches!(
            self,
            Self::FileWrite
                | Self::FilePatch
                | Self::ApplyPatch
                | Self::FileRemove
                | Self::FileMkdir
                | Self::FileCopy
                | Self::FileMove
                | Self::AgentSpawn
                | Self::TodoWrite
                | Self::MemoryWrite
                | Self::MissionCharterWrite
                | Self::PlanWrite
                | Self::KgWrite
                | Self::ValidationRecord
                | Self::Checkpoint
                | Self::HumanCheckpointRequest
        )
    }

    fn mutates_workspace_files(&self) -> bool {
        matches!(
            self,
            Self::FileWrite
                | Self::FilePatch
                | Self::ApplyPatch
                | Self::FileRemove
                | Self::FileMkdir
                | Self::FileCopy
                | Self::FileMove
        )
    }

    pub fn default_access_mode(&self) -> BuiltinToolAccessMode {
        if matches!(
            self,
            Self::ShellExec | Self::ProcessLaunch | Self::ProcessWrite | Self::ProcessKill
        ) {
            BuiltinToolAccessMode::MaybeWrite
        } else if self.is_write_operation() {
            BuiltinToolAccessMode::ExplicitWrite
        } else {
            BuiltinToolAccessMode::ReadOnly
        }
    }

    fn captures_workspace_changes(&self) -> bool {
        self.mutates_workspace_files() || matches!(self, Self::ShellExec)
    }

    pub fn is_public_tool_surface(&self) -> bool {
        !matches!(
            self,
            Self::ProcessLaunch
                | Self::ProcessRead
                | Self::ProcessWrite
                | Self::ProcessKill
                | Self::ProcessList
        )
    }

    pub fn is_runtime_internal_tool_call(&self) -> bool {
        matches!(
            self,
            Self::ProcessLaunch
                | Self::ProcessRead
                | Self::ProcessWrite
                | Self::ProcessKill
                | Self::ProcessList
                | Self::AgentSpawn
                | Self::AgentWait
                | Self::TodoWrite
                | Self::MemoryWrite
                | Self::MissionCharterWrite
                | Self::PlanWrite
                | Self::KgWrite
                | Self::ValidationRecord
                | Self::Checkpoint
                | Self::HumanCheckpointRequest
        )
    }

    pub fn is_session_timeline_renderable_tool_call(&self) -> bool {
        matches!(self, Self::AgentSpawn) || !self.is_runtime_internal_tool_call()
    }

    pub fn default_risk_level(&self) -> RiskLevel {
        match self {
            Self::FileRead
            | Self::ViewImage
            | Self::FileMkdir
            | Self::SearchText
            | Self::SearchSemantic
            | Self::ProcessRead
            | Self::ProcessList
            | Self::DiffPreview
            | Self::WebSearch
            | Self::WebFetch
            | Self::DiagramRender
            | Self::KnowledgeQuery
            | Self::CodeSymbols
            | Self::ToolCatalog
            | Self::AgentWait
            | Self::TodoWrite
            | Self::MemoryWrite
            | Self::MissionCharterWrite
            | Self::PlanWrite
            | Self::KgWrite
            | Self::ValidationRecord
            | Self::Checkpoint
            | Self::HumanCheckpointRequest => RiskLevel::Low,
            Self::FileWrite
            | Self::FilePatch
            | Self::ApplyPatch
            | Self::FileCopy
            | Self::FileMove
            | Self::ProcessWrite
            | Self::AgentSpawn => RiskLevel::Medium,
            Self::FileRemove | Self::ShellExec | Self::ProcessLaunch => RiskLevel::High,
            Self::ProcessKill | Self::ProcessInspect => RiskLevel::Medium,
        }
    }

    pub fn default_approval_requirement(&self) -> ApprovalRequirement {
        match self {
            Self::FileRemove | Self::ShellExec | Self::ProcessLaunch => {
                ApprovalRequirement::Required
            }
            _ => ApprovalRequirement::None,
        }
    }

    pub fn invocation_policy_for_input(&self, input: &str) -> BuiltinToolInvocationPolicy {
        match self {
            Self::ShellExec => shell_exec_invocation_policy(input),
            Self::FileRemove => file_remove_invocation_policy(input),
            _ => BuiltinToolInvocationPolicy {
                risk_level: self.default_risk_level(),
                approval_requirement: self.default_approval_requirement(),
            },
        }
    }

    pub fn uses_input_sensitive_invocation_policy(&self) -> bool {
        matches!(self, Self::ShellExec | Self::FileRemove)
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::FileRead => {
                "读取指定路径文件的内容。\n\n\
                # 何时用\n\
                - 需要查看文件内容、配置、源码以做出后续决策\n\
                - 读取已知具体路径的文件（含大文件可用 max_bytes 截取）\n\n\
                # 何时不用\n\
                - 列目录 / 找文件路径 → 用 shell_exec 跑 `ls` 或 `find`\n\
                - 跨文件搜索文本 → 用 search_text，不要逐个 file_read 后自己 grep\n\
                - 语义检索代码 → 用 search_semantic\n\n\
                # 反例\n\
                - ❌ 用 file_read 读取目录路径希望拿到列表\n\
                - ✅ 已经从 search_text 拿到候选文件路径后，再用 file_read 看具体行"
            }
            Self::ViewImage => {
                "读取本地图片并把图片内容作为多模态工具结果提供给模型。\n\n\
                # 何时用\n\
                - 需要查看截图、设计稿、照片、渲染结果或其他本地图片文件\n\
                - 用户给出图片路径，或代码生成了图片后需要视觉检查\n\n\
                # 何时不用\n\
                - 读取文本文件 → 用 file_read\n\
                - 列目录或查找图片文件路径 → 用 search_text 或 shell_exec 的只读命令\n\n\
                # 约束\n\
                - 只支持 png/jpeg/gif/webp\n\
                - 输出会同时包含审计 JSON 和模型可用的 image content；不要把 base64 当普通文本复述"
            }
            Self::FileWrite => {
                "创建或覆盖一个文件并写入指定内容（整体写入）。\n\n\
                # 何时用\n\
                - 创建新文件\n\
                - 对已有文件做完全重写（变化范围 ≥ 50% 或整体重构）\n\n\
                # 何时不用\n\
                - 修改已有文件中一段或几段 → 用 file_patch（保护未改部分、降低风险）\n\
                - 仅追加内容 → 用 file_patch 在末尾锚点做替换，或用 shell_exec 重定向\n\n\
                # 反例\n\
                - ❌ 把已有 500 行文件整段 file_write 回去只为改 3 行 → 极易丢失并发写入、破坏 diff\n\
                - ✅ 新建配置文件 / 新建源码模块时用 file_write\n\
                - ✅ 改 3 行用 file_patch，未改部分零风险"
            }
            Self::FilePatch => {
                "对文件进行精确文本替换（find-and-replace 风格的局部修改）。\n\n\
                # 何时用\n\
                - 修改已有文件中一段或几段（最常见的代码修改场景）\n\
                - old_string 必须在文件中精确出现一次；不唯一时先扩大上下文片段\n\n\
                # 何时不用\n\
                - 创建新文件 → 用 file_write\n\
                - 整体重构 / 改动量 ≥ 50% → 用 file_write 整体覆盖更清晰\n\
                - old_string 在文件中出现多次又不能扩展上下文 → 改用批量 patches 数组逐条精确替换\n\n\
                # 反例\n\
                - ❌ old_string 只写一行短代码且文件里出现多次 → 替换位置歧义、可能改错位置\n\
                - ✅ old_string 包含目标行 + 前后各 1-2 行上下文确保唯一性"
            }
            Self::ApplyPatch => {
                "应用 Codex 风格的补丁信封（*** Begin Patch / *** End Patch），支持新增、删除、更新和移动文件。\n\n\
                # 何时用\n\
                - 一次修改涉及多个文件或多个 hunk，需要用统一补丁表达完整 diff\n\
                - 需要生成接近代码审查 diff 的结构化改动，而不是单点 old_string 替换\n\n\
                # 何时不用\n\
                - 单文件单处精确替换 → 用 file_patch，约束更窄、失败更明确\n\
                - 整体创建或覆盖一个文件 → 用 file_write\n\n\
                # 输入格式\n\
                - 当前 function tool 传 JSON：{ \"patch\": \"*** Begin Patch\\n...\\n*** End Patch\\n\" }\n\
                - 未来 freeform 通道可直接传 patch 文本\n\
                - Update File 的上下文必须唯一匹配；不唯一时扩大上下文"
            }
            Self::FileRemove => "删除一个文件或目录",
            Self::FileMkdir => "创建一个目录（包含父目录）",
            Self::FileCopy => "把文件或目录复制到新位置",
            Self::FileMove => "移动或重命名文件 / 目录",
            Self::SearchText => "在指定目录下按文本模式搜索（grep 风格）",
            Self::SearchSemantic => "语义代码检索：基于自然语言描述定位相关代码",
            Self::ShellExec => {
                "执行一条 shell 命令并返回 stdout / stderr。\n\n\
                # 何时用\n\
                - 没有专用工具能完成的任务：构建（cargo build / npm run）、运行测试、git 操作、查 PID\n\
                - 一次性 ad-hoc 命令（解压、统计行数、查磁盘占用）\n\
                - 启动后台命令时设置 background=true；后续用同一个 shell_exec 传 action=read/write/kill/list 和 terminal_id 管理\n\n\
                # 何时不用\n\
                - 读文件内容 → file_read（更安全、有大小保护）\n\
                - 写文件内容 → file_write（避免引号转义陷阱）\n\
                - 改文件局部 → file_patch（避免 sed 转义灾难）\n\
                - 找文件路径 → 优先 search_text；search_text 不便时再 shell `find`\n\
                - 不要直接调用 process_launch / process_read / process_write / process_kill / process_list；它们是 shell_exec 的内部后台能力\n\n\
                # 反例\n\
                - ❌ `shell_exec: cat /path/file` 仅为读文件 → 失去字节限制保护\n\
                - ❌ `shell_exec: sed -i 's/foo/bar/'` 改文件 → 引号转义易错且不可预览\n\
                - ✅ `shell_exec: cargo test -p magi-agent-role` 跑测试\n\
                - ✅ `shell_exec: git log --oneline -20` 查提交历史"
            }
            Self::ProcessLaunch => "在当前会话 / 工作区启动一个后台进程",
            Self::ProcessRead => "读取受管后台进程的 stdout / stderr",
            Self::ProcessWrite => "向受管后台进程的 stdin 写入数据",
            Self::ProcessKill => "终止一个受管后台进程",
            Self::ProcessList => "列出当前上下文里的受管后台进程",
            Self::ProcessInspect => "按 PID 或名字查询正在运行的系统进程",
            Self::DiffPreview => "对两段文本生成 unified diff 预览",
            Self::WebSearch => "通过 DuckDuckGo 搜索网络并返回结果",
            Self::WebFetch => "抓取一个 URL 的内容并将 HTML 转为 markdown",
            Self::DiagramRender => {
                "渲染图表：支持 Mermaid、DOT、结构化 graph 节点/边、结构化 flow 节点/边"
            }
            Self::KnowledgeQuery => "查询项目知识库：检索 README、文档与代码文档",
            Self::CodeSymbols => {
                "代码符号导航：按符号名查定义（goto_definition），或列出某文件的全部符号（list_file_symbols）"
            }
            Self::ToolCatalog => {
                "列出 Magi 工具目录与健康状态。\n\n\
                # 何时用\n\
                - 需要确认当前运行时有哪些内置工具、skill 绑定工具、MCP server 状态、agent_spawn 可派发角色\n\
                - 需要诊断工具 schema 是否完整，或区分模型可见工具、外接工具与运行时内部工具\n\n\
                # 何时不用\n\
                - 已知道具体工具且要完成任务 → 直接调用对应工具\n\n\
                # 说明\n\
                - 内置工具读取 BuiltinToolName::ALL 单一注册源\n\
                - 外接工具只读取 daemon 注入的 skill/MCP 运行时快照，不扫描文件系统"
            }
            Self::AgentSpawn => {
                "向已注册的代理角色派发一个子任务（architect / executor / reviewer 等）。该工具只创建代理并投递初始任务消息，立即返回代理 task_id；后续使用 agent_wait 收集代理终态结果。若返回 status=degraded，表示代理当前不可用，父代理必须改派其他可用角色或由主线继续完成，不能直接停止任务。\n\n\
                # 何时用\n\
                - 任务可拆出 1 个或多个明确边界的子工作单元，且子单元能独立完成（有清晰输入、输出、验收）\n\
                - 需要专家视角（reviewer 做代码审查、explorer 做根因定位、tester 做验证）\n\
                - 多个子工作可并行执行节省时间\n\n\
                # 主线与代理分工\n\
                - 先判断当前关键路径应由主线直接推进，还是适合拆出代理并行工作\n\
                - 主线可以直接分析、读写文件、运行命令和验证；不要把 1-3 步即可完成的任务强行派发代理\n\
                - 代理适合处理边界清晰、可并行、不阻塞主线下一步的专项任务\n\
                - 代理运行中，主线应继续推进不重叠工作；不要空等，也不要重复做已经委派的同一件事\n\n\
                # 权限模式\n\
                - access_mode 必须表达本次代理是否允许写入：read_only 禁止写文件和写类 shell；read_write 按父任务策略允许必要写入\n\
                - 用户要求只读、审查、探索、方案分析、风险验证时使用 read_only\n\
                - 只有明确需要落地修改、生成文件、补测试或执行修复时才使用 read_write\n\n\
                # 何时不用\n\
                - 1-3 步能自己完成的任务 → 直接做，派发开销不值\n\
                - 子任务需要你在场即时回答澄清问题 → 自己做更顺\n\
                - 仅是查询性问题（找文件 / 读代码） → 用 search_text / file_read，不要派 agent\n\n\
                # display_name 写法\n\
                - 长度 3-30 个字符，前端代理卡片直接展示\n\
                - 如果用户明确给出了 display_name 或要求使用某个代理名称，必须原样使用该名称，不要自行改写、缩短或泛化\n\
                - 如果用户同时指定 role / display_name / access_mode，把这些值视为强制参数契约逐项转写；不要替换 role、不要重命名、不要把两个代理合并成一个\n\
                - 要让用户一眼看出『谁在做什么具体的事』，写成「职责 + 对象」短语\n\
                - ✅ 例：『登录流程审查员』『订单模块迁移设计师』『支付冒烟测试执行人』\n\
                - ❌ 反例：纯角色名『executor』『reviewer-1』；冗长重复『执行删除日志模块的所有引用并跑通测试的执行器』\n\n\
                # 反例\n\
                - ❌ 派 executor 去「改一行配置」→ 启动开销远超价值\n\
                - ❌ 派 reviewer 去「看看代码好不好」（边界模糊、验收不清）\n\
                - ✅ 派 reviewer 审查具体 PR：「审查 commits abc..def 的安全性，按通过 / 不通过给结论」\n\
                - ✅ 派 executor 实现独立模块：「在 crate X 实现 Y trait，跑通 cargo test -p X」\n\n\
                # 返回结果处理\n\
                - 返回 `status=started` 时，记录 `child_task_id`，后续通过 `agent_wait` 等待和收集结果\n\
                - 不要在依赖代理结果的情况下直接给最终答复；必须先调用 `agent_wait`\n\
                - agent_wait 返回 `child_status=completed` 时，`result.final_text` 是该代理的最终答复，`assignment.goal` 是你派给它的原始目标\n\
                - 同一轮多个代理返回后，先按任务合并结论、证据、风险与缺口，再生成主线最终答复；不要把多个代理输出原样拼贴给用户\n\
                - 返回 `status=degraded` 时代表代理不可用但主线必须继续：改派其他合适角色，或由主线基于已有上下文直接推进\n\
                - 返回 `status=failed` 时先判断是否可补救；能补救就重派或改派，只有真实阻断时才向用户说明失败"
            }
            Self::AgentWait => {
                "等待一个或多个已派发代理进入终态，并把代理最终答复作为结构化结果返回给主线。用于收集 agent_spawn 创建的代理结果；不要用轮询式重复调用，只有当下一步依赖代理结果时才调用。\n\n\
                # 参数\n\
                - task_ids：agent_spawn 返回的 child_task_id 列表\n\
                - timeout_ms：可选等待时长，默认 300000，范围 1000-1800000\n\n\
                # 返回结果处理\n\
                - `results[].child_status=completed`：读取 `results[].result.final_text` 并对照 `assignment.goal` 汇总\n\
                - `results[].child_status=failed/killed`：判断是否可改派或由主线接管，不要自动把单个代理失败当作整体失败\n\
                - `timed_out=true`：说明至少一个代理仍未完成；可以继续做不依赖该代理的工作，或稍后再次等待"
            }
            Self::TodoWrite => {
                "用给定列表整体替换当前会话的 TodoLedger（沿用 claude-code TodoWrite 语义）。用于把长任务拆分成步骤并跟踪进度；ledger 快照会自动注入到后续 Turn。每次调用整体覆盖。\n\n\
                # 何时用\n\
                - 任务 ≥ 3 个非平凡步骤，且步骤之间有先后关系或可能被打断\n\
                - 跨多轮对话推进、需要让用户随时看到进度\n\
                - 任务边界用户给得模糊，需要先拆解再让用户对齐\n\n\
                # 何时不用\n\
                - 单步任务（改一个文件、回答一个问题、跑一条命令）\n\
                - 纯查询 / 纯解释类对话（不会产出多步动作）\n\
                - 任务步骤太琐碎（每步 < 5 秒）→ todo 噪音超过价值\n\n\
                # 反例\n\
                - ❌ 「读一个文件」也建 todo → ledger 污染、降低后续 todo 信号价值\n\
                - ❌ 把『思考过程』当 todo（「想想 X」「分析 Y」）→ todo 应只记录可观察可验收的动作\n\
                - ✅ 实现一个跨多文件的功能：拆「读现状 / 改 A / 改 B / 跑测试 / commit」5 步\n\
                - ✅ 起步先写 todo 与用户对齐，确认后再开始执行"
            }
            Self::MemoryWrite => {
                "对当前工作区的 ProjectMemory 条目进行写入或删除。Memory 文件存于 ~/.magi/projects/<slug>/memory/，每次新会话开始时自动加载到系统提示。使用 action: save 进行 upsert（覆盖同 file_stem 的文件），action: delete 删除条目。Memory 类别：user / feedback / project / reference。"
            }
            Self::MissionCharterWrite => {
                "增量更新当前 mission 的章程（title / goal / success_criteria / constraints / stakeholders）。章程持久化在 ~/.magi/projects/<slug>/missions/<mission_id>/charter.md，会自动注入到 orchestrator 的提示词。至少提供一个字段；未提供的字段保持不变。"
            }
            Self::PlanWrite => {
                "用一份完整的步骤列表替换当前 mission 的执行计划。每个 step 包含：id（稳定标识）、content（一行描述）、status（pending / in_progress / completed / cancelled）、depends_on（可选的依赖 step id 列表）、notes（可选）。计划持久化在 ~/.magi/projects/<slug>/missions/<mission_id>/plan.md 并自动注入 orchestrator 提示词。用于起草、演进、跟踪 mission 的多步策略。每次调用整体覆盖——请把想保留的全部 step 都传进来。"
            }
            Self::KgWrite => {
                "向 mission 的 KnowledgeGraph 写入一条事实。Kind：'symbol'（代码 / 模块索引——哪些类与接口已经迁移、各自负责什么）、'decision'（架构或权衡决策，例如为什么选 SQLAlchemy 而非 Tortoise）、'risk'（执行中发现的隐患或注意点）。同一 (kind, id) 会覆盖旧事实并提升版本；设 'tombstoned': true 可在保留历史的前提下退役一条事实。KG 持久化在 ~/.magi/projects/<slug>/missions/<mission_id>/knowledge.md 并自动注入 orchestrator 提示词。"
            }
            Self::ValidationRecord => {
                "为某个 Plan step 记录一次验证结果。Kind：'test_suite'（单元 / 集成测试）、'type_check'（tsc / mypy / cargo check）、'integration_smoke'（跨进程或端到端冒烟）、'benchmark'（性能 / 负载）。Outcome：'pass' / 'fail' / 'skipped'。一次验证命令跑完后立即调用——Coordinator 要求每个 Plan step 至少有一次 Pass 且无未解决的 Fail 才能算完成。同一 (plan_step_id, kind) 会覆盖并提升版本。验证结果持久化在 ~/.magi/projects/<slug>/missions/<mission_id>/validation.md 并自动注入 orchestrator 提示词。"
            }
            Self::Checkpoint => {
                "为当前 mission 追加一条 Checkpoint 记录。Kind：'process_restart'（daemon 重启前后的状态记录）、'context_compaction'（对话刚被压缩，保留恢复指针）、'phase_transition'（一个主要 plan 阶段刚结束 / 开始）、'manual'（运维主动触发的安全网）。Checkpoint 仅追加；每条记录抓取一份 plan_version / kg_fact_count / workspace_commit / open conversations 的快照，未来某轮可以基于它推断如何恢复。持久化在 ~/.magi/projects/<slug>/missions/<mission_id>/checkpoints.md；最近几条会自动注入 orchestrator 提示词。"
            }
            Self::HumanCheckpointRequest => {
                "请求人工评审 checkpoint，并在运维批准或拒绝前暂停自主推进。用于高风险边界：不可逆操作、含糊的权衡、大规模删除、生产部署、任何需要人工判断的环节。必填：plan_step_id（触发请求的 Plan step）与 prompt_to_human（请运维处理的问题或决策）。可选：label（短标题）与 context（自由格式补充信息）。请求后不要再对该 mission 派发新工作；解决后再恢复。持久化在 ~/.magi/projects/<slug>/missions/<mission_id>/human_checkpoints.md；待处理 + 最近已处理条目会自动注入 orchestrator 提示词。"
            }
        }
    }

    pub fn parameters_schema(&self) -> serde_json::Value {
        match self {
            Self::FileRead => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要读取文件的绝对路径" },
                    "max_bytes": { "type": "integer", "description": "文件预览最多读取的字节数" }
                },
                "required": ["path"]
            }),
            Self::ViewImage => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要查看的本地图片路径，可为绝对路径或相对当前工作目录的路径" },
                    "max_bytes": { "type": "integer", "description": "允许读取的最大字节数，默认 10MiB，硬上限 20MiB" }
                },
                "required": ["path"]
            }),
            Self::FileWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要写入文件的绝对路径" },
                    "content": { "type": "string", "description": "要写入的文件内容" },
                    "overwrite": { "type": "boolean", "description": "是否覆盖已存在的文件（默认：true）" },
                    "create_dirs": { "type": "boolean", "description": "是否创建父目录（默认：true）" }
                },
                "required": ["path", "content"]
            }),
            Self::FilePatch => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要修改文件的绝对路径" },
                    "old_string": { "type": "string", "description": "要查找的原文本（必须在文件中精确匹配一次）" },
                    "new_string": { "type": "string", "description": "替换后的文本" },
                    "patches": {
                        "type": "array",
                        "description": "批量补丁数组（与 old_string / new_string 二选一）",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }),
            Self::ApplyPatch => serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "Codex 风格补丁信封，必须以 *** Begin Patch 开始并以 *** End Patch 结束。支持 *** Add File、*** Update File、*** Delete File，以及 Update File 下的 *** Move to。"
                    }
                },
                "required": ["patch"]
            }),
            Self::FileRemove => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要删除文件或目录的绝对路径" },
                    "recursive": { "type": "boolean", "description": "是否递归删除目录（默认：false）" }
                },
                "required": ["path"]
            }),
            Self::FileMkdir => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要创建目录的绝对路径" }
                },
                "required": ["path"]
            }),
            Self::FileCopy => serde_json::json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "源文件或目录的绝对路径" },
                    "destination": { "type": "string", "description": "目标位置的绝对路径" },
                    "overwrite": { "type": "boolean", "description": "目标存在时是否覆盖（默认：false）" }
                },
                "required": ["source", "destination"]
            }),
            Self::FileMove => serde_json::json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "源文件或目录的绝对路径" },
                    "destination": { "type": "string", "description": "目标位置的绝对路径" },
                    "overwrite": { "type": "boolean", "description": "目标存在时是否覆盖（默认：false）" }
                },
                "required": ["source", "destination"]
            }),
            Self::SearchText => serde_json::json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "搜索根目录" },
                    "query": { "type": "string", "description": "要搜索的文本模式" },
                    "limit": { "type": "integer", "description": "最大结果数" },
                    "case_sensitive": { "type": "boolean", "description": "是否区分大小写" },
                    "include_hidden": { "type": "boolean", "description": "是否包含隐藏文件与目录" }
                },
                "required": ["root", "query"]
            }),
            Self::SearchSemantic => serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "基于当前工作区本地代码索引检索的自然语言描述" },
                    "limit": { "type": "integer", "description": "最大结果数（默认：10）" }
                },
                "required": ["query"]
            }),
            Self::ShellExec => serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "cwd": { "type": "string", "description": "工作目录" },
                    "shell": { "type": "string", "description": "使用的 shell 程序" },
                    "timeout_ms": { "type": "integer", "description": "执行超时（毫秒）" },
                    "action": {
                        "type": "string",
                        "description": "后台进程控制动作：run/read/write/kill/list。省略时默认 run；background=true 启动后台命令后，使用 read/write/kill/list 搭配 terminal_id 继续管理。",
                        "enum": ["run", "read", "write", "kill", "list"]
                    },
                    "terminal_id": { "type": "integer", "description": "background=true 返回的受管终端 / 进程 ID；action=read/write/kill 时必填" },
                    "input": { "type": "string", "description": "action=write 时写入后台进程 stdin 的文本" },
                    "content": { "type": "string", "description": "input 的别名" },
                    "text": { "type": "string", "description": "input 的别名" },
                    "max_bytes": { "type": "integer", "description": "action=read 时 stdout / stderr 预览最多读取的字节数" },
                    "access_mode": {
                        "type": "string",
                        "description": "声明命令访问模式：read_only / maybe_write / explicit_write。ls、cat、grep、git status、git diff、不会改文件的测试等只读探查请用 read_only。只读探测中“文件不存在/无匹配”属于可汇报结果时，命令必须用 if/else、|| true 或末尾 true 保证整体退出码为 0，避免把可恢复探测误判为任务失败。",
                        "enum": ["read_only", "maybe_write", "explicit_write"]
                    },
                    "background": { "type": "boolean", "description": "在后台启动而不阻塞等待完成" }
                },
                "required": []
            }),
            Self::ProcessLaunch => serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "在后台启动的 shell 命令" },
                    "cwd": { "type": "string", "description": "工作目录" },
                    "shell": { "type": "string", "description": "使用的 shell 程序" }
                },
                "required": ["command"]
            }),
            Self::ProcessRead => serde_json::json!({
                "type": "object",
                "properties": {
                    "terminal_id": { "type": "integer", "description": "受管终端 / 进程 ID" },
                    "max_bytes": { "type": "integer", "description": "stdout / stderr 预览最多读取的字节数" }
                },
                "required": ["terminal_id"]
            }),
            Self::ProcessWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "terminal_id": { "type": "integer", "description": "受管终端 / 进程 ID" },
                    "input": { "type": "string", "description": "要写入进程 stdin 的文本" },
                    "content": { "type": "string", "description": "input 的别名" },
                    "text": { "type": "string", "description": "input 的别名" }
                },
                "required": ["terminal_id"]
            }),
            Self::ProcessKill => serde_json::json!({
                "type": "object",
                "properties": {
                    "terminal_id": { "type": "integer", "description": "受管终端 / 进程 ID" }
                },
                "required": ["terminal_id"]
            }),
            Self::ProcessList => serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            Self::ProcessInspect => serde_json::json!({
                "type": "object",
                "properties": {
                    "pid": { "type": "integer", "description": "要检查的进程 ID" },
                    "query": { "type": "string", "description": "进程名或搜索关键词" },
                    "name": { "type": "string", "description": "query 的别名" },
                    "pattern": { "type": "string", "description": "query 的别名" },
                    "limit": { "type": "integer", "description": "最大匹配条数" }
                },
                "required": []
            }),
            Self::DiffPreview => serde_json::json!({
                "type": "object",
                "properties": {
                    "before": { "type": "string", "description": "原始文本" },
                    "after": { "type": "string", "description": "修改后文本" },
                    "before_path": { "type": "string", "description": "原始文件路径" },
                    "after_path": { "type": "string", "description": "更新后文件路径" },
                    "before_label": { "type": "string", "description": "原始侧的标签" },
                    "after_label": { "type": "string", "description": "更新侧的标签" },
                    "left": { "type": "string", "description": "before 的别名" },
                    "right": { "type": "string", "description": "after 的别名" },
                    "left_path": { "type": "string", "description": "before_path 的别名" },
                    "right_path": { "type": "string", "description": "after_path 的别名" },
                    "left_label": { "type": "string", "description": "before_label 的别名" },
                    "right_label": { "type": "string", "description": "after_label 的别名" }
                },
                "required": []
            }),
            Self::WebSearch => serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "搜索关键词" }
                },
                "required": ["query"]
            }),
            Self::WebFetch => serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "要抓取内容的 URL" },
                    "prompt": { "type": "string", "description": "可选的抓取提示词或抽取指令" }
                },
                "required": ["url"]
            }),
            Self::DiagramRender => serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["mermaid", "dot", "graph", "flow"],
                        "description": "图表输入类型。flow 用于思维导图、层级结构、步骤和流程节点图；graph 用于关系/网络图；mermaid 仅用于 Mermaid 特定语法（sequence/state/gantt/pie/class/ER/timeline/quadrant/requirement/C4/sankey/xychart/block 等），不要使用 Mermaid mindmap；dot 用于 DOT 语法。"
                    },
                    "source": { "type": "string", "description": "mermaid 或 dot 类型的图表源码。产品侧不支持 Mermaid mindmap；思维导图请用 kind=flow 或 kind=graph，配合 graph.nodes/edges 表达。" },
                    "graph": {
                        "type": "object",
                        "description": "graph 或 flow 类型使用的结构化图数据。思维导图请把中心主题作为第一个/根节点，并用显式 edges 连接子主题。",
                        "properties": {
                            "nodes": {
                                "type": "array",
                                "description": "节点列表，每个节点包含 id、label，及可选的 group/data 字段",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "description": "稳定的节点 id" },
                                        "label": { "type": "string", "description": "可读的节点标签" },
                                        "group": { "type": "string", "description": "可选的节点分组" },
                                        "position": {
                                            "type": "object",
                                            "properties": {
                                                "x": { "type": "number" },
                                                "y": { "type": "number" }
                                            }
                                        },
                                        "data": { "type": "object", "description": "可选的渲染器节点元数据" }
                                    },
                                    "required": ["id"]
                                }
                            },
                            "edges": {
                                "type": "array",
                                "description": "边列表，每条边包含 source、target，及可选的 label/data 字段",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "description": "可选的稳定边 id" },
                                        "source": { "type": "string", "description": "起点节点 id" },
                                        "target": { "type": "string", "description": "终点节点 id" },
                                        "label": { "type": "string", "description": "可读的边标签" },
                                        "data": { "type": "object", "description": "可选的渲染器边元数据" }
                                    },
                                    "required": ["source", "target"]
                                }
                            }
                        },
                        "required": ["nodes", "edges"]
                    },
                    "title": { "type": "string", "description": "可选的图表标题" },
                    "layout": {
                        "type": "string",
                        "enum": ["auto", "dagre", "elk", "tidy-tree", "cose", "force", "fcose", "cose-bilkent", "grid", "circle", "preset"],
                        "description": "偏好布局。auto 让渲染器根据 kind 自行决定合适的布局。"
                    },
                    "interactive": { "type": "boolean", "description": "渲染器是否启用平移、缩放和节点交互（若支持）" },
                    "theme": { "type": "string", "description": "图表主题提示" }
                },
                "required": ["kind"]
            }),
            Self::KnowledgeQuery => serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "用于检索当前工作区知识库的自然语言问题" },
                    "kind": {
                        "type": "string",
                        "enum": ["all", "adr", "faq", "learning", "code_index"],
                        "description": "知识类型过滤，默认 all"
                    },
                    "tags": {
                        "type": "array",
                        "description": "可选标签过滤",
                        "items": { "type": "string" }
                    },
                    "limit": { "type": "integer", "description": "最多返回多少个匹配（默认 10，最大 50）" }
                },
                "required": ["query"]
            }),
            Self::CodeSymbols => serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["definition", "file_symbols"],
                        "description": "definition：按符号名查定义；file_symbols：列出某文件的全部符号"
                    },
                    "name": { "type": "string", "description": "action=definition 时的符号名（函数/类/接口/类型等）" },
                    "path": { "type": "string", "description": "action=file_symbols 时的文件路径（相对工作区根）" },
                    "limit": { "type": "integer", "description": "definition 最多返回多少个匹配（默认 20）" }
                },
                "required": ["action"]
            }),
            Self::ToolCatalog => serde_json::json!({
                "type": "object",
                "properties": {
                    "include_internal": { "type": "boolean", "description": "是否包含运行时内部工具，默认 false" },
                    "include_schema": { "type": "boolean", "description": "是否在每个内置工具条目中包含完整 parameters_schema，默认 false" },
                    "include_external": { "type": "boolean", "description": "是否包含 skill 绑定工具与 MCP server 快照，默认 true" },
                    "include_mcp_servers": { "type": "boolean", "description": "包含外接工具时是否同时返回 MCP server 健康摘要，默认 true" },
                    "include_agent_roles": { "type": "boolean", "description": "是否包含可通过 agent_spawn 派发的代理角色健康摘要，默认 true" }
                },
                "required": []
            }),
            Self::AgentSpawn => serde_json::json!({
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "已注册的代理角色 id，如 architect / executor / explorer / reviewer / tester。不要传 coordinator，主线协调身份由当前主模型承接。若用户明确指定 role，必须原样使用，不得替换成你认为更接近的角色。" },
                    "display_name": { "type": "string", "description": "本次派发的代理实例展示名（3-30 个字符），用于前端代理卡片标题。若用户明确给出 display_name 或指定代理名称，必须原样使用；不得自行改写、缩短、泛化或把两个指定代理合并。否则要求高度概括本次具体职责，例如『登录流程审查员』『支付迁移设计师』『冒烟测试执行人』；不要写成纯角色名（如『executor』）或冗长目标重复。" },
                    "goal": { "type": "string", "description": "子任务的具体目标；角色级 system prompt 会与该目标合并使用" },
                    "access_mode": {
                        "type": "string",
                        "enum": ["read_only", "read_write"],
                        "description": "本次代理的权限模式。read_only 禁止写工具和写类 shell；read_write 按父任务策略允许必要写入。用户要求只读、审查、探索、方案分析或风险验证时必须用 read_only。"
                    },
                    "task_kind": {
                        "type": "string",
                        "enum": ["work_package", "action", "validation", "repair"],
                        "description": "新建子任务的类型。省略时默认 action。"
                    },
                    "context": { "type": "string", "description": "可选的上下文摘要，传递给子 agent（单一字符串）。" },
                    "working_dir": { "type": "string", "description": "可选的绝对工作目录；默认沿用父任务的 workspace 根目录" },
                    "parallelism_group": { "type": "string", "description": "可选的并行组名；同一 SpawnGraph 分支下相同组名的子 agent 互斥执行" }
                },
                "required": ["role", "display_name", "goal", "access_mode"]
            }),
            Self::AgentWait => serde_json::json!({
                "type": "object",
                "properties": {
                    "task_ids": {
                        "type": "array",
                        "description": "要等待的代理 task_id 列表，来自 agent_spawn 返回的 child_task_id",
                        "items": { "type": "string" }
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "可选等待时长，默认 300000，范围 1000-1800000"
                    }
                },
                "required": ["task_ids"]
            }),
            Self::TodoWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "新的完整 todo 列表。每次调用整表覆盖当前账本。",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "祈使句描述的步骤，例如 'Run tests'"
                                },
                                "activeForm": {
                                    "type": "string",
                                    "description": "in_progress 状态下展示的进行时形式，例如 'Running tests'"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "步骤状态。同时只允许有一个步骤处于 in_progress。"
                                }
                            },
                            "required": ["content", "activeForm", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
            Self::MemoryWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["save", "delete"],
                        "description": "save: 新增或更新一条记忆；delete: 按 file_stem 删除已有记忆。"
                    },
                    "file_stem": {
                        "type": "string",
                        "description": "不含扩展名的文件名，只允许 [A-Za-z0-9_-]。保留名 MEMORY 会被拒绝。"
                    },
                    "name": {
                        "type": "string",
                        "description": "用于 MEMORY.md 索引的简短标题。action=save 时必填。"
                    },
                    "description": {
                        "type": "string",
                        "description": "用于索引的一行式描述，说明这条记忆是关于什么的。action=save 时必填。"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["user", "feedback", "project", "reference"],
                        "description": "记忆分类。action=save 时必填。"
                    },
                    "body": {
                        "type": "string",
                        "description": "完整的 markdown 正文。action=save 时必填。"
                    }
                },
                "required": ["action", "file_stem"]
            }),
            Self::MissionCharterWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "描述本 mission 交付什么的简短标题。"
                    },
                    "goal": {
                        "type": "string",
                        "description": "完整阐述用户意图，以及 mission 承诺达成的结果。"
                    },
                    "success_criteria": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "可验证的条目，定义 mission 何时算完成。"
                    },
                    "constraints": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "限定 mission 边界的硬约束（范围 / 技术 / 时间 / 策略）。"
                    },
                    "stakeholders": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "需要被尊重的相关人或角色。"
                    }
                },
                "required": []
            }),
            Self::PlanWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "description": "完整有序的计划步骤列表。每次调用整表替换现有计划。",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "稳定标识符（如 's1'、'audit-deps'）。供 depends_on 引用。"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "本步骤目标的一行描述。"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "cancelled"],
                                    "description": "步骤状态；省略时默认 'pending'。"
                                },
                                "depends_on": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "可选的依赖步骤 id 列表，所有 id 必须在 steps 中存在。"
                                },
                                "notes": {
                                    "type": "string",
                                    "description": "本步骤的可选理由或备注。"
                                }
                            },
                            "required": ["id", "content"]
                        }
                    }
                },
                "required": ["steps"]
            }),
            Self::KgWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["symbol", "decision", "risk"],
                        "description": "事实分类。'symbol' 用于代码/模块事实；'decision' 用于 ADR 风格的决策；'risk' 用于需要关注的隐患或约束。"
                    },
                    "id": {
                        "type": "string",
                        "description": "分类内的稳定 id。复用同一 (kind, id) 会覆盖旧事实并提升版本号。"
                    },
                    "content": {
                        "type": "string",
                        "description": "用一至几句话陈述该事实。"
                    },
                    "reference": {
                        "type": "string",
                        "description": "可选指针：文件路径、URL、ADR id 等事实来源。"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "可选的自由标签，便于后续检索/过滤。"
                    },
                    "tombstoned": {
                        "type": "boolean",
                        "description": "置为 true 表示废弃。被废弃的事实仍保留在磁盘上，但不再注入 prompt。"
                    }
                },
                "required": ["kind", "id", "content"]
            }),
            Self::ValidationRecord => serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_step_id": {
                        "type": "string",
                        "description": "本次验证覆盖的 Plan 步骤 id，必须匹配当前 mission plan.md 中的某个步骤。"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["test_suite", "type_check", "integration_smoke", "benchmark"],
                        "description": "验证类型：单元/集成测试、静态类型检查、端到端冒烟、性能基准。"
                    },
                    "outcome": {
                        "type": "string",
                        "enum": ["pass", "fail", "skipped"],
                        "description": "验证结果。一个 Plan 步骤至少有一次 pass 且没有未解决的 fail 时才算完成。"
                    },
                    "command": {
                        "type": "string",
                        "description": "可选：产生本次结果的命令行（如 'cargo test -p magi-api'），便于后续复现。"
                    },
                    "evidence": {
                        "type": "string",
                        "description": "可选：本次运行的简短摘要（通过用例数、失败断言、性能数值等）——保持 diff 友好。"
                    }
                },
                "required": ["plan_step_id", "kind", "outcome"]
            }),
            Self::Checkpoint => serde_json::json!({
                "type": "object",
                "description": "追加一条 Mission 检查点记录。恢复类型（process_restart / context_compaction / phase_transition）必须携带非空的 workspace_commit，且 open_conversations 中的每一项都必须指向 recovery_ref 或 execution_chain_ref ——不完整的恢复集会被拒绝。",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["process_restart", "context_compaction", "phase_transition", "manual"],
                        "description": "检查点分类。process_restart = daemon 重启边界；context_compaction = 会话刚被压缩；phase_transition = 一个 Plan 阶段边界；manual = 操作者触发（只有 manual 允许跳过恢复集）。"
                    },
                    "label": {
                        "type": "string",
                        "description": "简短可读的标签，方便后续读者快速挑选正确的检查点。"
                    },
                    "plan_version": {
                        "type": "integer",
                        "description": "可选：本检查点捕获的 plan 版本号（恢复时可与当前 plan diff）。"
                    },
                    "kg_fact_count": {
                        "type": "integer",
                        "description": "可选：本检查点处 mission KG 事实总数。"
                    },
                    "workspace_commit": {
                        "type": "string",
                        "description": "本检查点捕获的 workspace VCS commit / ref。恢复类型必填；仅 kind=manual 时可选。"
                    },
                    "open_conversations": {
                        "type": "array",
                        "description": "session 恢复指针列表。每项必须包含 session_id 以及 recovery_ref / execution_chain_ref 至少其一，使运行时能在重启后重建活跃 ExecutionChain / mailbox。",
                        "items": {
                            "type": "object",
                            "properties": {
                                "session_id": {
                                    "type": "string",
                                    "description": "需要可恢复 ExecutionChain 的会话标识。"
                                },
                                "recovery_ref": {
                                    "type": "string",
                                    "description": "指向 session-store 恢复 sidecar（Conversation/Mailbox 快照）的指针。recovery_ref 与 execution_chain_ref 至少存在其一。"
                                },
                                "execution_chain_ref": {
                                    "type": "string",
                                    "description": "指向活跃 ExecutionChain 日志的指针，便于恢复后重建当前任务链。"
                                },
                                "turn_cursor": {
                                    "type": "integer",
                                    "description": "可选：最后应用的轮次游标——用于检测 checkpoint 与恢复 sidecar 之间的漂移。"
                                },
                                "pending_mailbox": {
                                    "type": "integer",
                                    "description": "可选：仍未处理的 mailbox 条目数——帮助操作者判断恢复是否安全。"
                                }
                            },
                            "required": ["session_id"]
                        }
                    },
                    "notes": {
                        "type": "string",
                        "description": "可选的自由备注（例如为何记录此检查点、恢复时需注意什么）。"
                    }
                },
                "required": ["kind"]
            }),
            Self::HumanCheckpointRequest => serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_step_id": {
                        "type": "string",
                        "description": "触发本请求的 Plan 步骤标识（必须匹配已有的 plan 步骤 id）。"
                    },
                    "prompt_to_human": {
                        "type": "string",
                        "description": "需要操作者解决的问题或决策。请明确：可选项、权衡、以及当前语境下 'approve' 与 'reject' 各自意味着什么。"
                    },
                    "label": {
                        "type": "string",
                        "description": "可选的短标题，与待决请求一起展示在操作者面板上。"
                    },
                    "context": {
                        "type": "string",
                        "description": "可选的自由补充上下文（链接、片段、前置决策等），供操作者参考。"
                    }
                },
                "required": ["plan_step_id", "prompt_to_human"]
            }),
        }
    }
}

pub fn is_public_builtin_tool_surface(name: &str) -> bool {
    BuiltinToolName::from_str(name)
        .map(|tool| tool.is_public_tool_surface())
        .unwrap_or(false)
}

pub fn is_internal_builtin_tool_surface(name: &str) -> bool {
    BuiltinToolName::from_str(name)
        .map(|tool| !tool.is_public_tool_surface())
        .unwrap_or(false)
}

pub fn canonical_builtin_tool_name(name: &str) -> Option<String> {
    BuiltinToolName::from_str(name.trim()).map(|tool| tool.as_str().to_string())
}

pub fn builtin_permission_engine() -> magi_permissions::PermissionEngine {
    let mut engine = magi_permissions::PermissionEngine::default();
    for tool in BuiltinToolName::ALL {
        match tool.default_access_mode() {
            BuiltinToolAccessMode::ReadOnly => engine.register_read_only_tool(tool.as_str()),
            BuiltinToolAccessMode::ExplicitWrite => engine.register_edit_tool(tool.as_str()),
            BuiltinToolAccessMode::MaybeWrite => {}
        }
    }
    engine
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuiltinToolSpec {
    pub name: String,
    pub risk_level: RiskLevel,
    pub approval_requirement: ApprovalRequirement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinToolInvocationPolicy {
    pub risk_level: RiskLevel,
    pub approval_requirement: ApprovalRequirement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinToolAccessMode {
    ReadOnly,
    MaybeWrite,
    ExplicitWrite,
}

impl BuiltinToolAccessMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::MaybeWrite => "maybe_write",
            Self::ExplicitWrite => "explicit_write",
        }
    }

    fn is_writeful(&self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "read" | "read_only" | "readonly" => Some(Self::ReadOnly),
            "maybe" | "maybe_write" | "maybewrite" => Some(Self::MaybeWrite),
            "write" | "explicit_write" | "explicitwrite" => Some(Self::ExplicitWrite),
            _ => None,
        }
    }
}

fn low_risk_policy() -> BuiltinToolInvocationPolicy {
    BuiltinToolInvocationPolicy {
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
    }
}

fn medium_risk_policy() -> BuiltinToolInvocationPolicy {
    BuiltinToolInvocationPolicy {
        risk_level: RiskLevel::Medium,
        approval_requirement: ApprovalRequirement::None,
    }
}

fn high_risk_approval_policy() -> BuiltinToolInvocationPolicy {
    BuiltinToolInvocationPolicy {
        risk_level: RiskLevel::High,
        approval_requirement: ApprovalRequirement::Required,
    }
}

fn shell_exec_invocation_policy(input: &str) -> BuiltinToolInvocationPolicy {
    let Some(request) = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        return medium_risk_policy();
    };

    let action = json_field_string(&request, &["action", "operation", "op"])
        .map(|value| value.trim().to_ascii_lowercase());
    let has_terminal_id = request
        .get("terminal_id")
        .or_else(|| request.get("terminalId"))
        .or_else(|| request.get("id"))
        .is_some();
    let has_command = json_field_string(&request, &["command", "script", "line"])
        .is_some_and(|value| !value.trim().is_empty());

    match action.as_deref() {
        None if has_terminal_id && !has_command => return low_risk_policy(),
        Some("read" | "poll" | "status" | "list" | "ls") => return low_risk_policy(),
        Some("write" | "stdin" | "send" | "kill" | "stop" | "terminate" | "cancel") => {
            return medium_risk_policy();
        }
        Some("run" | "exec" | "command") | None => {}
        Some(_) => return medium_risk_policy(),
    }

    if json_field_bool(&request, &["background", "long_running", "longRunning"]).unwrap_or(false) {
        return medium_risk_policy();
    }

    match json_field_string(&request, &["access_mode", "write_mode", "intent"])
        .and_then(|value| BuiltinToolAccessMode::from_str(&value))
    {
        Some(BuiltinToolAccessMode::ReadOnly)
            if magi_permissions::PermissionEngine::shell_arguments_request_read_only(input) =>
        {
            low_risk_policy()
        }
        Some(BuiltinToolAccessMode::MaybeWrite | BuiltinToolAccessMode::ExplicitWrite) => {
            medium_risk_policy()
        }
        Some(BuiltinToolAccessMode::ReadOnly) | None => medium_risk_policy(),
    }
}

fn file_remove_invocation_policy(input: &str) -> BuiltinToolInvocationPolicy {
    let Some(request) = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        return medium_risk_policy();
    };

    if json_field_bool(&request, &["recursive", "force"]).unwrap_or(false) {
        return high_risk_approval_policy();
    }

    if json_field_string(&request, &["path", "file_path", "target_path"])
        .is_some_and(|value| is_high_risk_remove_target(&value))
    {
        return high_risk_approval_policy();
    }

    medium_risk_policy()
}

fn json_field_string(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    })
}

fn json_field_bool(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<bool> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| {
            value
                .as_bool()
                .or_else(|| value.as_str().and_then(|value| value.parse::<bool>().ok()))
        })
    })
}

fn is_high_risk_remove_target(path: &str) -> bool {
    matches!(path.trim(), "/" | "." | ".." | "~")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutionInput {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub input: String,
    pub approval_requirement: ApprovalRequirement,
    pub risk_level: RiskLevel,
}

impl ToolExecutionInput {
    pub fn for_builtin_invocation(
        tool_call_id: ToolCallId,
        requested_tool_name: impl AsRef<str>,
        input: impl Into<String>,
    ) -> Self {
        let input = input.into();
        let requested_tool_name = requested_tool_name.as_ref().trim();
        let (tool_name, invocation_policy) =
            if let Some(tool) = BuiltinToolName::from_str(requested_tool_name) {
                (
                    tool.as_str().to_string(),
                    tool.invocation_policy_for_input(&input),
                )
            } else {
                (requested_tool_name.to_string(), low_risk_policy())
            };

        Self {
            tool_call_id,
            tool_name,
            tool_kind: ToolKind::Builtin,
            input,
            approval_requirement: invocation_policy.approval_requirement,
            risk_level: invocation_policy.risk_level,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionContext {
    pub worker_id: Option<WorkerId>,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionContextQuery {
    pub worker_id: Option<WorkerId>,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteProtectionScope {
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub task_id: Option<TaskId>,
    pub working_directory: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutionOutput {
    pub tool_call_id: ToolCallId,
    pub status: ExecutionResultStatus,
    pub payload: String,
    pub governance: GovernanceDecision,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionPolicy {
    pub access_profile: magi_core::AccessProfile,
    pub source_skill_ids: Vec<String>,
    pub allowed_tool_names: Vec<String>,
    pub denied_tool_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInvocationRecord {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub context: ToolExecutionContext,
    pub status: ExecutionResultStatus,
    pub payload: String,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionSummary {
    pub total_invocations: usize,
    pub successful_invocations: usize,
    pub blocked_invocations: usize,
    pub failed_invocations: usize,
}

/// 工具执行时可用的进程内运行时资源（非序列化句柄）。
///
/// 与 ToolExecutionContext（可序列化、随调用流转的标识信息）区分：
/// 这里承载的是 daemon 进程内的共享服务引用，由 ToolRegistry 持有并在
/// dispatch 时传入。将来需要更多运行时服务，扩这个结构即可，不改 trait 签名。
pub type ExternalToolCatalogProvider =
    Arc<dyn Fn() -> ExternalToolCatalogSnapshot + Send + Sync + 'static>;
pub type AgentRoleCatalogProvider =
    Arc<dyn Fn() -> Vec<AgentRoleCatalogEntry> + Send + Sync + 'static>;
pub type RuntimeCapabilityDependencyProvider = Arc<
    dyn Fn(&ToolExecutionContext) -> Vec<RuntimeCapabilityDependencyEntry> + Send + Sync + 'static,
>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExternalToolCatalogSnapshot {
    pub skill_tools: Vec<ExternalToolCatalogEntry>,
    pub mcp_servers: Vec<ExternalMcpServerCatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalToolCatalogEntry {
    pub source: String,
    pub skill_id: Option<String>,
    pub binding_id: Option<String>,
    pub name: String,
    pub description: String,
    pub bridge_kind: String,
    pub dispatch_action: String,
    pub bridge_target: String,
    pub access_profile_behavior: String,
    pub risk_level: String,
    pub approval_requirement: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalMcpServerCatalogEntry {
    pub server_id: String,
    pub name: String,
    pub enabled: bool,
    pub connected: bool,
    pub health: String,
    pub tool_count: Option<usize>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRoleCatalogEntry {
    pub role_id: String,
    pub spawnable: bool,
    pub coordinator_mode: bool,
    pub supported_kinds: Vec<String>,
    pub parallelism_limit: Option<u32>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeCapabilityDependencyEntry {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_indexed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawnable_role_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<usize>,
}

#[derive(Clone, Default)]
pub struct ToolRuntimeResources {
    pub knowledge_store: Option<Arc<magi_knowledge_store::KnowledgeStore>>,
    pub external_tool_catalog_provider: Option<ExternalToolCatalogProvider>,
    pub agent_role_catalog_provider: Option<AgentRoleCatalogProvider>,
    pub runtime_capability_dependency_provider: Option<RuntimeCapabilityDependencyProvider>,
}

pub trait BuiltinTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(
        &self,
        input: &str,
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
    ) -> String;
    fn spec(&self) -> BuiltinToolSpec;
}

#[derive(Clone)]
pub struct ToolRegistry {
    governance: Arc<GovernanceService>,
    event_bus: Arc<InMemoryEventBus>,
    builtin_tools: HashMap<String, Arc<dyn BuiltinTool>>,
    invocations: Arc<RwLock<Vec<ToolInvocationRecord>>>,
    active_write_claims: Arc<RwLock<HashMap<ToolCallId, WriteProtectionClaim>>>,
    runtime_resources: ToolRuntimeResources,
}

impl ToolRegistry {
    pub fn new(governance: Arc<GovernanceService>, event_bus: Arc<InMemoryEventBus>) -> Self {
        Self {
            governance,
            event_bus,
            builtin_tools: HashMap::new(),
            invocations: Arc::new(RwLock::new(Vec::new())),
            active_write_claims: Arc::new(RwLock::new(HashMap::new())),
            runtime_resources: ToolRuntimeResources::default(),
        }
    }

    /// 注入 KnowledgeStore，让代码检索工具走真正的本地索引引擎。
    pub fn with_knowledge_store(
        mut self,
        knowledge_store: Arc<magi_knowledge_store::KnowledgeStore>,
    ) -> Self {
        self.runtime_resources.knowledge_store = Some(knowledge_store);
        self
    }

    pub fn with_external_tool_catalog_provider(
        mut self,
        provider: ExternalToolCatalogProvider,
    ) -> Self {
        self.runtime_resources.external_tool_catalog_provider = Some(provider);
        self
    }

    pub fn with_agent_role_catalog_provider(mut self, provider: AgentRoleCatalogProvider) -> Self {
        self.runtime_resources.agent_role_catalog_provider = Some(provider);
        self
    }

    pub fn with_runtime_capability_dependency_provider(
        mut self,
        provider: RuntimeCapabilityDependencyProvider,
    ) -> Self {
        self.runtime_resources
            .runtime_capability_dependency_provider = Some(provider);
        self
    }

    pub fn register_builtin(&mut self, tool: Arc<dyn BuiltinTool>) {
        self.builtin_tools.insert(tool.name().to_string(), tool);
    }

    pub fn register_default_builtins(&mut self) {
        for name in BuiltinToolName::ALL {
            self.register_builtin(Arc::new(NormalizedBuiltinTool::new(
                name,
                name.default_risk_level(),
                name.default_approval_requirement(),
            )));
        }
    }

    pub fn builtin_specs(&self) -> Vec<BuiltinToolSpec> {
        let mut specs = Vec::with_capacity(self.builtin_tools.len());
        for name in BuiltinToolName::ALL {
            if let Some(tool) = self.builtin_tools.get(name.as_str()) {
                specs.push(tool.spec());
            }
        }
        let mut custom_tools = self
            .builtin_tools
            .iter()
            .filter(|(name, _)| {
                !BuiltinToolName::ALL
                    .iter()
                    .any(|builtin| builtin.as_str() == name.as_str())
            })
            .collect::<Vec<_>>();
        custom_tools.sort_by(|(left, _), (right, _)| left.cmp(right));
        specs.extend(custom_tools.into_iter().map(|(_, tool)| tool.spec()));
        specs
    }

    pub fn public_builtin_specs(&self) -> Vec<BuiltinToolSpec> {
        self.builtin_specs()
            .into_iter()
            .filter(|spec| is_public_builtin_tool_surface(&spec.name))
            .collect()
    }

    pub fn tool_catalog_value(
        &self,
        input: &str,
        context: &ToolExecutionContext,
    ) -> serde_json::Value {
        tool_catalog::build_tool_catalog_value(input, context, &self.runtime_resources)
    }

    pub fn builtin_access_mode(&self, tool_name: &str) -> Option<BuiltinToolAccessMode> {
        self.builtin_tools
            .get(tool_name)
            .and_then(|_| BuiltinToolName::from_str(tool_name))
            .map(|name| name.default_access_mode())
    }

    /// 根据允许/拒绝列表创建过滤后的工具注册表副本。
    pub fn filtered_clone(&self, allowed: &[String], denied: &[String]) -> Self {
        let mut filtered = self.clone();
        if !allowed.is_empty() {
            filtered
                .builtin_tools
                .retain(|name, _| allowed.contains(name));
        }
        if !denied.is_empty() {
            filtered
                .builtin_tools
                .retain(|name, _| !denied.contains(name));
        }
        filtered
    }

    pub fn execute(&self, input: ToolExecutionInput) -> ToolExecutionOutput {
        self.execute_with_context(input, ToolExecutionContext::default())
    }

    pub fn execute_with_context(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
    ) -> ToolExecutionOutput {
        self.execute_with_policy(input, context, &ToolExecutionPolicy::default())
    }

    pub fn execute_with_policy(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, false)
    }

    pub fn cancel_active_shell_execs(&self, query: &ToolExecutionContextQuery) -> usize {
        builtin::cancel_active_shell_execs(query)
    }

    #[cfg(test)]
    fn execute_internal_builtin_with_policy(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, true)
    }

    fn execute_with_policy_for_surface(
        &self,
        mut input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
        allow_internal_builtin_surface: bool,
    ) -> ToolExecutionOutput {
        if input.tool_kind == ToolKind::Builtin
            && let Some(canonical_name) = BuiltinToolName::from_str(input.tool_name.trim())
        {
            input.tool_name = canonical_name.as_str().to_string();
            if !allow_internal_builtin_surface && !canonical_name.is_public_tool_surface() {
                let output = self.build_internal_builtin_surface_rejection(&input, canonical_name);
                self.record_invocation(&input, &context, &output);
                return output;
            }
        }
        if let Some(output) = self.enforce_execution_policy(&input, policy) {
            self.record_invocation(&input, &context, &output);
            return output;
        }
        let access_mode = self.resolve_access_mode(&input);
        if let Some(output) = self.enforce_access_profile_policy(&input, policy, access_mode) {
            self.record_invocation(&input, &context, &output);
            return output;
        }

        let governance = if policy.access_profile == magi_core::AccessProfile::FullAccess {
            GovernanceDecision::allowed(
                DecisionPhase::ApprovalPolicy,
                input.risk_level,
                Some("完全授权模式跳过普通工具审批".to_string()),
            )
        } else {
            self.governance
                .evaluate_tool_request(&ToolExecutionRequest {
                    tool_name: input.tool_name.clone(),
                    tool_kind: input.tool_kind.clone(),
                    risk_level: input.risk_level,
                    approval_requirement: input.approval_requirement,
                })
        };

        let output = if !governance.allowed {
            ToolExecutionOutput {
                tool_call_id: input.tool_call_id.clone(),
                status: if governance.requires_approval {
                    ExecutionResultStatus::NeedsApproval
                } else {
                    ExecutionResultStatus::Rejected
                },
                payload: governance
                    .reason
                    .clone()
                    .unwrap_or_else(|| "工具调用被阻断".to_string()),
                governance,
            }
        } else {
            match self.builtin_tools.get(&input.tool_name) {
                Some(tool) => {
                    let write_guard = match self.acquire_write_guard(&input, &context, access_mode)
                    {
                        Ok(guard) => guard,
                        Err(output) => {
                            self.record_invocation(&input, &context, &output);
                            return output;
                        }
                    };
                    let before_changes = capture_tool_workspace_snapshot(&input, &context);
                    let payload = tool.execute(&input.input, &context, &self.runtime_resources);
                    let payload =
                        append_workspace_changed_paths(payload, before_changes.as_ref(), &context);
                    drop(write_guard);
                    ToolExecutionOutput {
                        tool_call_id: input.tool_call_id.clone(),
                        status: infer_execution_status(&payload),
                        payload,
                        governance,
                    }
                }
                None => ToolExecutionOutput {
                    tool_call_id: input.tool_call_id.clone(),
                    status: ExecutionResultStatus::Failed,
                    payload: format!("未注册的工具: {}", input.tool_name),
                    governance,
                },
            }
        };

        self.record_invocation(&input, &context, &output);
        output
    }

    fn build_internal_builtin_surface_rejection(
        &self,
        input: &ToolExecutionInput,
        tool_name: BuiltinToolName,
    ) -> ToolExecutionOutput {
        let reason = format!(
            "{} 是 shell_exec 的内部运行时能力，不接受模型、worker 或外部调用直接执行；需要后台终端时请调用 shell_exec(background=true)",
            tool_name.as_str()
        );
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: ExecutionResultStatus::Rejected,
            payload: serde_json::json!({
                "tool": tool_name.as_str(),
                "status": "rejected",
                "error": reason.clone(),
            })
            .to_string(),
            governance: GovernanceDecision {
                outcome: GovernanceOutcome::Rejected,
                allowed: false,
                requires_approval: false,
                phase: DecisionPhase::ToolPolicy,
                threshold: input.risk_level,
                reason: Some(reason),
            },
        }
    }

    pub fn invocations(&self) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .clone()
    }

    pub fn summary(&self) -> ToolExecutionSummary {
        self.summary_for_query(&ToolExecutionContextQuery::default())
    }

    pub fn query_invocations(
        &self,
        query: &ToolExecutionContextQuery,
    ) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .iter()
            .filter(|record| {
                query
                    .worker_id
                    .as_ref()
                    .is_none_or(|id| record.context.worker_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .task_id
                    .as_ref()
                    .is_none_or(|id| record.context.task_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .session_id
                    .as_ref()
                    .is_none_or(|id| record.context.session_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .workspace_id
                    .as_ref()
                    .is_none_or(|id| record.context.workspace_id.as_ref() == Some(id))
            })
            .cloned()
            .collect()
    }

    pub fn governance_decision_for_tool_request(
        &self,
        request: &ToolExecutionRequest,
        access_profile: AccessProfile,
    ) -> GovernanceDecision {
        if access_profile == AccessProfile::FullAccess {
            return GovernanceDecision::allowed(
                DecisionPhase::ApprovalPolicy,
                request.risk_level,
                Some("完全授权模式跳过普通工具审批".to_string()),
            );
        }
        self.governance.evaluate_tool_request(request)
    }

    pub fn record_external_invocation(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        output: &ToolExecutionOutput,
    ) {
        self.record_invocation(input, context, output);
    }

    pub fn summary_for_query(&self, query: &ToolExecutionContextQuery) -> ToolExecutionSummary {
        let invocations = self.query_invocations(query);
        let invocations = self.summarize_invocations(&invocations);
        invocations
    }

    fn summarize_invocations(&self, invocations: &[ToolInvocationRecord]) -> ToolExecutionSummary {
        let total_invocations = invocations.len();
        let successful_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Succeeded)
            .count();
        let blocked_invocations = invocations
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    ExecutionResultStatus::NeedsApproval | ExecutionResultStatus::Rejected
                )
            })
            .count();
        let failed_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Failed)
            .count();
        ToolExecutionSummary {
            total_invocations,
            successful_invocations,
            blocked_invocations,
            failed_invocations,
        }
    }

    fn record_invocation(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        output: &ToolExecutionOutput,
    ) {
        let record = ToolInvocationRecord {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            tool_kind: input.tool_kind.clone(),
            context: context.clone(),
            status: output.status,
            payload: output.payload.clone(),
            created_at: UtcMillis::now(),
        };
        self.invocations
            .write()
            .expect("tool invocation write lock poisoned")
            .push(record.clone());
        self.publish_with_category(
            "tool.invoked",
            EventCategory::Audit,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                task_id: record.context.task_id.clone(),
                ..EventContext::default()
            },
            EventId::new(format!("tool-call-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
        self.publish_with_category(
            "tool.usage.recorded",
            EventCategory::Usage,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                mission_id: None,
                assignment_id: None,
                task_id: record.context.task_id.clone(),
            },
            EventId::new(format!("tool-usage-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "risk_level": format!("{:?}", input.risk_level),
                "approval_requirement": format!("{:?}", input.approval_requirement),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
    }
}

impl ToolRegistry {
    fn publish_with_category(
        &self,
        event_type: &str,
        category: EventCategory,
        context: EventContext,
        event_id: EventId,
        payload: serde_json::Value,
    ) {
        let envelope = match category {
            EventCategory::Domain => EventEnvelope::domain(event_id, event_type, payload),
            EventCategory::Audit => EventEnvelope::audit(event_id, event_type, payload),
            EventCategory::Usage => EventEnvelope::usage(event_id, event_type, payload),
            EventCategory::Projection => EventEnvelope::projection(event_id, event_type, payload),
            EventCategory::System => EventEnvelope::system(event_id, event_type, payload),
        };
        let _ = self.event_bus.publish(envelope.with_context(context));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceChangeSnapshot {
    root: PathBuf,
    files: BTreeMap<String, WorkspaceFileFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceFileFingerprint {
    status_code: String,
    content_hash: Option<u64>,
    exists: bool,
    is_dir: bool,
}

const FILESYSTEM_SNAPSHOT_MAX_FILES: usize = 5000;
const FILESYSTEM_SNAPSHOT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

fn capture_tool_workspace_snapshot(
    input: &ToolExecutionInput,
    context: &ToolExecutionContext,
) -> Option<WorkspaceChangeSnapshot> {
    if input.tool_kind != ToolKind::Builtin {
        return None;
    }
    let tool_name = BuiltinToolName::from_str(input.tool_name.trim())?;
    if !tool_name.captures_workspace_changes() {
        return None;
    }
    capture_workspace_change_snapshot(context.working_directory.as_deref()?)
}

fn capture_workspace_change_snapshot(workdir: &Path) -> Option<WorkspaceChangeSnapshot> {
    if let Some(repo_root) = run_git_capture(workdir, &["rev-parse", "--show-toplevel"])
        .map(|root| PathBuf::from(root.trim()))
        .filter(|root| !root.as_os_str().is_empty())
    {
        let status = run_git_capture(
            &repo_root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        let mut files = BTreeMap::new();
        for line in status.lines() {
            let Some((status_code, file_path)) = parse_git_status_path(line) else {
                continue;
            };
            files.insert(
                file_path.clone(),
                fingerprint_workspace_file(&repo_root, &file_path, &status_code),
            );
        }
        return Some(WorkspaceChangeSnapshot {
            root: repo_root,
            files,
        });
    }

    capture_filesystem_change_snapshot(workdir)
}

fn capture_filesystem_change_snapshot(root: &Path) -> Option<WorkspaceChangeSnapshot> {
    let root = root
        .canonicalize()
        .ok()
        .or_else(|| Some(root.to_path_buf()))?;
    let mut files = BTreeMap::new();
    collect_filesystem_fingerprints(&root, &root, &mut files);
    Some(WorkspaceChangeSnapshot { root, files })
}

fn append_workspace_changed_paths(
    payload: String,
    before: Option<&WorkspaceChangeSnapshot>,
    context: &ToolExecutionContext,
) -> String {
    let Some(before) = before else {
        return payload;
    };
    let Some(after) = capture_workspace_change_snapshot(
        context
            .working_directory
            .as_deref()
            .unwrap_or_else(|| before.root.as_path()),
    ) else {
        return payload;
    };
    if after.root != before.root {
        return payload;
    }
    let changed_paths = workspace_changed_paths(before, &after);
    if changed_paths.is_empty() {
        return payload;
    }
    append_changed_paths_to_json_payload(payload, &changed_paths)
}

fn workspace_changed_paths(
    before: &WorkspaceChangeSnapshot,
    after: &WorkspaceChangeSnapshot,
) -> Vec<String> {
    before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.files.get(path) != after.files.get(path))
        .collect()
}

fn append_changed_paths_to_json_payload(payload: String, changed_paths: &[String]) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&payload) else {
        return payload;
    };
    let Some(object) = value.as_object_mut() else {
        return payload;
    };

    let mut merged = object
        .get("changed_paths")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    merged.extend(changed_paths.iter().cloned());
    object.insert(
        "changed_paths".to_string(),
        serde_json::Value::Array(merged.into_iter().map(serde_json::Value::String).collect()),
    );
    value.to_string()
}

fn run_git_capture(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_git_status_path(line: &str) -> Option<(String, String)> {
    if line.len() < 4 {
        return None;
    }
    let status_code = line.get(..2)?.to_string();
    let path_segment = line.get(3..)?.trim();
    if path_segment.is_empty() {
        return None;
    }
    let file_path = path_segment
        .rsplit(" -> ")
        .next()
        .map(str::trim)
        .filter(|path| !path.is_empty())?
        .to_string();
    Some((status_code, file_path))
}

fn fingerprint_workspace_file(
    repo_root: &Path,
    file_path: &str,
    status_code: &str,
) -> WorkspaceFileFingerprint {
    let absolute_path = repo_root.join(file_path);
    match fs::metadata(&absolute_path) {
        Ok(metadata) => WorkspaceFileFingerprint {
            status_code: status_code.to_string(),
            content_hash: if metadata.is_file() {
                hash_file_contents(&absolute_path)
            } else {
                None
            },
            exists: true,
            is_dir: metadata.is_dir(),
        },
        Err(_) => WorkspaceFileFingerprint {
            status_code: status_code.to_string(),
            content_hash: None,
            exists: false,
            is_dir: false,
        },
    }
}

fn collect_filesystem_fingerprints(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<String, WorkspaceFileFingerprint>,
) {
    if files.len() >= FILESYSTEM_SNAPSHOT_MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= FILESYSTEM_SNAPSHOT_MAX_FILES {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if filesystem_snapshot_should_skip_dir(&path) {
                continue;
            }
            collect_filesystem_fingerprints(root, &path, files);
            continue;
        }
        if !metadata.is_file() || metadata.len() > FILESYSTEM_SNAPSHOT_MAX_FILE_BYTES {
            continue;
        }
        let Some(relative_path) = path
            .strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        files.insert(
            relative_path,
            WorkspaceFileFingerprint {
                status_code: "FS".to_string(),
                content_hash: hash_file_contents(&path),
                exists: true,
                is_dir: false,
            },
        );
    }
}

fn filesystem_snapshot_should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | "coverage" | ".next" | ".svelte-kit"
    )
}

fn hash_file_contents(path: &Path) -> Option<u64> {
    let bytes = fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        ApprovalRequirement, RiskLevel, SessionId, TaskId, ToolCallId, WorkerId, WorkspaceId,
    };
    use magi_governance::{DecisionPhase, GovernanceOutcome};
    use serde_json::Value;
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        process::Command,
        sync::Arc,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{}-{}-{}", name, std::process::id(), suffix));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn all_builtin_tools() -> [BuiltinToolName; BuiltinToolName::ALL.len()] {
        BuiltinToolName::ALL
    }

    #[test]
    fn file_read_supports_raw_path_and_directory_listing() {
        let root = unique_temp_dir("magi-tool-file-read");
        let file_path = root.join("hello.txt");
        fs::write(&file_path, "hello\nworld").expect("write file");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-file-read"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file_path.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "file_read");
        assert_eq!(payload["access_mode"], "read_only");
        assert_eq!(payload["mode"], "file");
        assert_eq!(payload["truncated"], false);
        assert!(
            payload["content"]
                .as_str()
                .expect("content")
                .contains("hello")
        );

        let dir_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-file-read-dir"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "path": root.to_string_lossy(),
                    "max_bytes": 8
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(dir_output.status, ExecutionResultStatus::Succeeded);
        let dir_payload: Value =
            serde_json::from_str(&dir_output.payload).expect("dir payload json");
        assert_eq!(dir_payload["mode"], "directory");
        assert_eq!(dir_payload["entries"].as_array().expect("entries").len(), 1);
    }

    #[test]
    fn builtin_execution_emits_usage_event_and_updates_ledger() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-usage"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "/tmp/nonexistent".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);

        let snapshot = event_bus.snapshot();
        let usage_events = snapshot
            .recent_events
            .iter()
            .filter(|event| event.category == EventCategory::Usage)
            .collect::<Vec<_>>();
        assert!(!usage_events.is_empty());
        let usage_payload = &usage_events[0].payload;
        assert_eq!(usage_payload["tool_name"], "file_read");
        assert_eq!(usage_payload["status"], "Failed");
        assert_eq!(usage_payload["risk_level"], "Low");

        let ledger_status = event_bus.audit_usage_ledger_status();
        assert!(ledger_status.usage_count >= 1);
        assert_eq!(ledger_status.audit_count, 1);
    }

    #[test]
    fn search_text_supports_json_input() {
        let root = unique_temp_dir("magi-tool-search");
        fs::create_dir_all(root.join("nested")).expect("nested");
        fs::write(root.join("nested").join("one.txt"), "alpha\nneedle\nbeta").expect("write");
        fs::write(root.join("two.txt"), "needle here too").expect("write");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-search"),
                tool_name: BuiltinToolName::SearchText.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "root": root.to_string_lossy(),
                    "query": "needle",
                    "limit": 10,
                    "case_sensitive": true
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "search_text");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(
            payload["returned_matches"]
                .as_u64()
                .expect("returned matches")
                >= 2
        );
        assert!(!payload["matches"].as_array().expect("matches").is_empty());
    }

    #[test]
    fn shell_exec_runs_and_reports_failure_semantics() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &full_access_policy(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "shell_exec");
        assert_eq!(payload["access_mode"], "maybe_write");
        assert_eq!(payload["stdout"], "hello");

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-blocked"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf blocked".to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(blocked.status, ExecutionResultStatus::NeedsApproval);
    }

    #[test]
    fn shell_exec_rejects_read_only_mode_with_write_redirection() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let root = unique_temp_dir("magi-tool-shell-read-only-redirection");
        let target = root.join("should-not-exist.txt");

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-read-only-redirection"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": format!("printf hidden > {}", target.display()),
                    "access_mode": "read_only"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext {
                working_directory: Some(root.clone()),
                ..ToolExecutionContext::default()
            },
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Rejected);
        assert!(!target.exists(), "read_only shell 不应执行写入重定向");
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "shell_exec");
        assert_eq!(payload["status"], "rejected");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_exec_background_process_can_be_controlled_through_shell_surface() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let root = unique_temp_dir("magi-tool-shell-background-control");
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-background-control")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-background-control")),
            working_directory: Some(root),
            ..ToolExecutionContext::default()
        };

        let launch = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-background-launch"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf ready; sleep 5",
                    "background": true
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &full_access_policy(),
        );

        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
        let launch_payload: Value = serde_json::from_str(&launch.payload).expect("launch json");
        assert_eq!(launch_payload["tool"], "shell_exec");
        assert_eq!(launch_payload["mode"], "background");
        let terminal_id = launch_payload["terminal_id"]
            .as_u64()
            .expect("terminal_id should be returned");

        thread::sleep(Duration::from_millis(100));
        let read = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-background-read"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "action": "read",
                    "terminal_id": terminal_id,
                    "max_bytes": 1024
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &full_access_policy(),
        );

        assert_eq!(read.status, ExecutionResultStatus::Succeeded);
        let read_payload: Value = serde_json::from_str(&read.payload).expect("read json");
        assert_eq!(read_payload["tool"], "shell_exec");
        assert_eq!(read_payload["mode"], "background_read");
        assert!(
            read_payload["stdout"]
                .as_str()
                .expect("stdout")
                .contains("ready")
        );

        let list = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-background-list"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "action": "list" }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &full_access_policy(),
        );
        let list_payload: Value = serde_json::from_str(&list.payload).expect("list json");
        assert_eq!(list_payload["mode"], "background_list");
        assert!(
            list_payload["processes"]
                .as_array()
                .expect("processes")
                .iter()
                .any(|process| process["terminal_id"].as_u64() == Some(terminal_id))
        );

        let kill = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-background-kill"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "action": "kill",
                    "terminal_id": terminal_id
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &full_access_policy(),
        );

        assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
        let kill_payload: Value = serde_json::from_str(&kill.payload).expect("kill json");
        assert_eq!(kill_payload["tool"], "shell_exec");
        assert_eq!(kill_payload["mode"], "background_kill");
    }

    #[test]
    fn shell_exec_read_only_git_status_in_non_git_workspace_is_stable_probe() {
        let root = unique_temp_dir("magi-tool-shell-non-git-probe");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-non-git")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-non-git")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-non-git-status"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "git status --short",
                    "access_mode": "read_only"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["exit_code"], 0);
        assert_eq!(payload["git_worktree"], false);
        assert_eq!(payload["stdout"], "NOT_GIT_WORKTREE\n");
    }

    #[test]
    fn shell_exec_read_only_compound_git_status_in_non_git_workspace_is_stable_probe() {
        let root = unique_temp_dir("magi-tool-shell-compound-non-git-probe");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-compound-non-git")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-compound-non-git")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };
        let command = format!(
            "pwd && printf '\\n---\\n' && ls -1 {} | head -n 3 && printf '\\n---\\n' && git -C {} status --short",
            root.display(),
            root.display()
        );

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-compound-non-git-status"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": command,
                    "access_mode": "read_only"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["exit_code"], 0);
        assert_eq!(payload["git_worktree"], false);
        assert_eq!(payload["stdout"], "NOT_GIT_WORKTREE\n");
    }

    #[test]
    fn shell_exec_records_git_worktree_changed_paths() {
        let root = unique_temp_dir("magi-tool-shell-change-capture");
        Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init should run");
        Command::new("git")
            .args(["config", "user.email", "codex@example.com"])
            .current_dir(&root)
            .output()
            .expect("git email config should run");
        Command::new("git")
            .args(["config", "user.name", "Codex"])
            .current_dir(&root)
            .output()
            .expect("git name config should run");
        fs::write(root.join("tracked-a.txt"), "alpha\n").expect("tracked a should write");
        fs::write(root.join("tracked-b.txt"), "beta\n").expect("tracked b should write");
        Command::new("git")
            .args(["add", "--", "tracked-a.txt", "tracked-b.txt"])
            .current_dir(&root)
            .output()
            .expect("git add should run");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&root)
            .output()
            .expect("git commit should run");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-change-capture")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-change-capture")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-change-capture"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf 'alpha changed\\n' > tracked-a.txt && rm tracked-b.txt && mkdir -p tmp && printf 'new file\\n' > tmp/new-a.txt",
                    "access_mode": "explicit_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &full_access_policy(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        let changed_paths = payload["changed_paths"]
            .as_array()
            .expect("changed paths should be recorded")
            .iter()
            .map(|value| value.as_str().expect("path should be string"))
            .collect::<Vec<_>>();
        assert!(changed_paths.contains(&"tracked-a.txt"));
        assert!(changed_paths.contains(&"tracked-b.txt"));
        assert!(changed_paths.contains(&"tmp/new-a.txt"));
    }

    #[test]
    fn shell_exec_records_non_git_filesystem_changed_paths() {
        let root = unique_temp_dir("magi-tool-shell-non-git-change-capture");
        fs::write(root.join("remove-me.txt"), "remove me\n").expect("seed file should write");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-non-git-change")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-non-git-change")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-non-git-change"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf 'new file\\n' > new-a.txt && rm remove-me.txt",
                    "access_mode": "explicit_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &full_access_policy(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        let changed_paths = payload["changed_paths"]
            .as_array()
            .expect("changed paths should be recorded")
            .iter()
            .map(|value| value.as_str().expect("path should be string"))
            .collect::<Vec<_>>();
        assert!(changed_paths.contains(&"new-a.txt"));
        assert!(changed_paths.contains(&"remove-me.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn shell_exec_cancel_active_session_kills_running_command() {
        let registry = make_registry();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-shell-cancel")),
            session_id: Some(SessionId::new("session-shell-cancel")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-cancel")),
            working_directory: None,
        };
        let runner_registry = registry.clone();
        let runner_context = context.clone();
        let runner = std::thread::spawn(move || {
            runner_registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new("tool-call-shell-cancel"),
                    tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: serde_json::json!({
                        "command": "sleep 2",
                        "timeout_ms": 5000
                    })
                    .to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                runner_context,
                &full_access_policy(),
            )
        });

        std::thread::sleep(Duration::from_millis(100));
        let cancel_started = Instant::now();
        let cancelled = registry.cancel_active_shell_execs(&ToolExecutionContextQuery {
            session_id: context.session_id.clone(),
            workspace_id: context.workspace_id.clone(),
            task_id: None,
            worker_id: None,
        });

        assert_eq!(cancelled, 1);
        let output = runner.join().expect("shell execution thread should join");
        assert!(
            cancel_started.elapsed() < Duration::from_millis(1500),
            "取消 shell_exec 后不应等待 sleep 自然结束"
        );
        assert_eq!(output.status, ExecutionResultStatus::Cancelled);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload should parse");
        assert_eq!(payload["tool"], "shell_exec");
        assert_eq!(payload["cancelled"], true);
    }

    #[test]
    fn shell_exec_rejects_blank_json_command() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-blank"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "command": "   " }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(output.payload.contains("缺少 shell 命令"));
    }

    #[test]
    fn builtin_required_fields_reject_empty_json_objects() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let cases = [
            ("file_read", "缺少文件路径"),
            ("search_text", "缺少搜索关键词"),
            ("shell_exec", "缺少 shell 命令"),
            ("file_remove", "缺少文件路径"),
            ("file_mkdir", "缺少目录路径"),
            ("web_search", "缺少搜索关键词 query"),
            ("web_fetch", "缺少 URL"),
            ("search_semantic", "缺少 query 字段"),
            ("knowledge_query", "缺少 query 字段"),
        ];

        for (tool_name, expected_error) in cases {
            let output = tool_registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new(format!("tool-call-empty-{tool_name}")),
                    tool_name: tool_name.to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: serde_json::json!({}).to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                ToolExecutionContext::default(),
                &ToolExecutionPolicy::default(),
            );

            assert_eq!(
                output.status,
                ExecutionResultStatus::Failed,
                "{tool_name} should reject empty JSON object"
            );
            assert!(
                output.payload.contains(expected_error),
                "{tool_name} payload should contain {expected_error}, got {}",
                output.payload
            );
        }
    }

    #[test]
    fn builtins_use_context_working_directory_for_relative_inputs() {
        let root = unique_temp_dir("magi-tool-context-cwd");
        fs::write(root.join("marker.txt"), "workspace-marker").expect("write marker");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let shell_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-context-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "test -f marker.txt && printf workspace-ok",
                    "access_mode": "read_only"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        let shell_payload: Value =
            serde_json::from_str(&shell_output.payload).expect("shell payload should parse");
        assert_eq!(shell_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(shell_payload["stdout"], "workspace-ok");

        let file_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-context-file-read"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "marker.txt".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        let file_payload: Value =
            serde_json::from_str(&file_output.payload).expect("file payload should parse");
        assert_eq!(file_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            file_payload["path"],
            root.join("marker.txt").display().to_string()
        );
        assert_eq!(file_payload["content"], "workspace-marker");

        let search_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-context-search"),
                tool_name: BuiltinToolName::SearchText.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "query": "workspace-marker" }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );
        let search_payload: Value =
            serde_json::from_str(&search_output.payload).expect("search payload should parse");
        assert_eq!(search_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(search_payload["root"], root.display().to_string());
        assert_eq!(search_payload["returned_matches"], 1);
    }

    #[test]
    fn shell_exec_blocks_conflicting_write_scope_until_guard_drops() {
        let root = unique_temp_dir("magi-tool-shell-write");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-write")),
            session_id: Some(SessionId::new("session-write")),
            workspace_id: Some(WorkspaceId::new("workspace-write")),
            working_directory: None,
        };
        let guarded_input = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-write-guard"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf guarded",
                "cwd": root.to_string_lossy(),
                "access_mode": "explicit_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };

        let write_guard = tool_registry
            .acquire_write_guard(
                &guarded_input,
                &context,
                BuiltinToolAccessMode::ExplicitWrite,
            )
            .expect("guard acquisition")
            .expect("writeful guard");

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-write-blocked"),
                ..guarded_input.clone()
            },
            context.clone(),
            &full_access_policy(),
        );
        assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
        let blocked_payload: Value =
            serde_json::from_str(&blocked.payload).expect("blocked payload json");
        assert_eq!(blocked_payload["tool"], "shell_exec");
        assert_eq!(blocked_payload["access_mode"], "explicit_write");
        assert!(
            blocked_payload["error"]
                .as_str()
                .expect("blocked error")
                .contains("并发写冲突")
        );

        drop(write_guard);

        let allowed =
            tool_registry.execute_with_policy(guarded_input, context, &full_access_policy());
        assert_eq!(allowed.status, ExecutionResultStatus::Succeeded);
    }

    #[test]
    fn write_guard_tracks_file_copy_camel_case_destination_alias() {
        let root = unique_temp_dir("magi-tool-copy-write-guard");
        let source = root.join("source.txt");
        let destination = root.join("target.txt");
        fs::write(&source, "source").expect("source file should write");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let guarded_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("copy-task")),
            session_id: Some(SessionId::new("session-copy-guard")),
            workspace_id: Some(WorkspaceId::new("workspace-copy-guard")),
            working_directory: None,
        };
        let blocked_context = ToolExecutionContext {
            task_id: Some(TaskId::new("write-task")),
            ..guarded_context.clone()
        };
        let guarded_input = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-copy-guard"),
            tool_name: BuiltinToolName::FileCopy.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "sourcePath": source.to_string_lossy(),
                "destinationPath": destination.to_string_lossy()
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };

        let write_guard = tool_registry
            .acquire_write_guard(
                &guarded_input,
                &guarded_context,
                BuiltinToolAccessMode::ExplicitWrite,
            )
            .expect("guard acquisition")
            .expect("writeful guard");

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-copy-guard-blocked-write"),
                tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "filePath": destination.to_string_lossy(),
                    "content": "blocked"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            blocked_context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
        let blocked_payload: Value =
            serde_json::from_str(&blocked.payload).expect("blocked payload json");
        assert!(
            blocked_payload["error"]
                .as_str()
                .expect("blocked error")
                .contains("并发写冲突")
        );
        assert!(!destination.exists());

        drop(write_guard);
    }

    #[test]
    fn shell_exec_isolates_write_guards_by_workspace_and_session() {
        let root = unique_temp_dir("magi-tool-shell-workdir");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let guarded_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-a")),
            session_id: Some(SessionId::new("session-a")),
            workspace_id: Some(WorkspaceId::new("workspace-a")),
            working_directory: None,
        };
        let other_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-b")),
            session_id: Some(SessionId::new("session-b")),
            workspace_id: Some(WorkspaceId::new("workspace-b")),
            working_directory: None,
        };
        let guarded_input = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-workdir-guard"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf guarded",
                "cwd": root.to_string_lossy(),
                "access_mode": "maybe_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };

        let write_guard = tool_registry
            .acquire_write_guard(
                &guarded_input,
                &guarded_context,
                BuiltinToolAccessMode::MaybeWrite,
            )
            .expect("guard acquisition")
            .expect("writeful guard");

        let allowed_other_context = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-workdir-other-context"),
                ..guarded_input.clone()
            },
            other_context,
            &full_access_policy(),
        );
        assert_eq!(
            allowed_other_context.status,
            ExecutionResultStatus::Succeeded
        );

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-workdir-blocked"),
                ..guarded_input.clone()
            },
            guarded_context,
            &full_access_policy(),
        );
        assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
        let blocked_payload: Value =
            serde_json::from_str(&blocked.payload).expect("blocked payload json");
        assert_eq!(blocked_payload["access_mode"], "maybe_write");
        assert!(
            blocked_payload["error"]
                .as_str()
                .expect("blocked error")
                .contains("并发写冲突")
        );

        drop(write_guard);
    }

    #[test]
    fn process_inspect_reports_current_process() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let current_pid = std::process::id();
        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process"),
                tool_name: BuiltinToolName::ProcessInspect.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: current_pid.to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "process_inspect");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(
            payload["matches"]
                .as_array()
                .expect("matches")
                .iter()
                .any(|item| {
                    item["pid"]
                        .as_u64()
                        .map(|pid| pid as u32 == current_pid)
                        .unwrap_or(false)
                })
        );
    }

    #[test]
    fn process_launch_does_not_block_followup_shell_in_same_session() {
        let root = unique_temp_dir("magi-tool-process-launch");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-launch")),
            session_id: Some(SessionId::new("session-process-launch")),
            workspace_id: Some(WorkspaceId::new("workspace-process-launch")),
            working_directory: None,
        };

        let launch = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 2",
                    "cwd": root.to_string_lossy(),
                    "access_mode": "maybe_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &full_access_policy(),
        );
        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
        let launch_payload: Value =
            serde_json::from_str(&launch.payload).expect("launch payload json");
        let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

        let started = Instant::now();
        let followup = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-followup-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf followup",
                    "cwd": root.to_string_lossy(),
                    "access_mode": "maybe_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &full_access_policy(),
        );

        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "后台进程不应阻塞后续 shell"
        );
        assert_eq!(followup.status, ExecutionResultStatus::Succeeded);
        let followup_payload: Value =
            serde_json::from_str(&followup.payload).expect("followup payload json");
        assert_eq!(followup_payload["stdout"], "followup");

        let kill = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-kill"),
                tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &full_access_policy(),
        );
        assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
    }

    #[test]
    fn process_launch_rejects_blank_json_command() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-blank"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "command": "   " }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &full_access_policy(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(output.payload.contains("缺少 shell 命令"));
    }

    #[test]
    fn process_tools_reject_missing_session_or_workspace_context() {
        let root = unique_temp_dir("magi-tool-process-context");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch-no-context"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 1",
                    "cwd": root.to_string_lossy()
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &full_access_policy(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(output.payload.contains("需要 session 或 workspace 上下文"));

        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-context")),
            session_id: Some(SessionId::new("session-process-context")),
            workspace_id: Some(WorkspaceId::new("workspace-process-context")),
            working_directory: None,
        };
        let launch = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch-context"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 2",
                    "cwd": root.to_string_lossy()
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &full_access_policy(),
        );
        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
        let launch_payload: Value =
            serde_json::from_str(&launch.payload).expect("launch payload json");
        let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

        let read_without_context = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-read-no-context"),
                tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(read_without_context.status, ExecutionResultStatus::Failed);
        assert!(
            read_without_context
                .payload
                .contains("需要 session 或 workspace 上下文")
        );

        let kill = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-kill-context"),
                tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &full_access_policy(),
        );
        assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
    }

    #[test]
    fn process_tools_do_not_cross_sessions_with_workspace_only_context() {
        let root = unique_temp_dir("magi-tool-process-session-scope");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let owner_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-owner")),
            session_id: Some(SessionId::new("session-process-owner")),
            workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
            working_directory: None,
        };
        let workspace_only_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-workspace-only")),
            session_id: None,
            workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
            working_directory: None,
        };
        let other_session_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-other")),
            session_id: Some(SessionId::new("session-process-other")),
            workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
            working_directory: None,
        };

        let launch = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch-owner"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 2",
                    "cwd": root.to_string_lossy()
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            owner_context.clone(),
            &full_access_policy(),
        );
        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
        let launch_payload: Value =
            serde_json::from_str(&launch.payload).expect("launch payload json");
        let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

        let read_workspace_only = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-read-workspace-only"),
                tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            workspace_only_context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(read_workspace_only.status, ExecutionResultStatus::Failed);
        assert!(
            read_workspace_only
                .payload
                .contains("进程不属于当前 session/workspace")
        );

        let read_other_session = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-read-other-session"),
                tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            other_session_context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(read_other_session.status, ExecutionResultStatus::Failed);
        assert!(
            read_other_session
                .payload
                .contains("进程不属于当前 session/workspace")
        );

        let process_list = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-list-workspace-only"),
                tool_name: BuiltinToolName::ProcessList.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({}).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            workspace_only_context,
            &ToolExecutionPolicy::default(),
        );
        let list_payload: Value =
            serde_json::from_str(&process_list.payload).expect("list payload json");
        assert_eq!(process_list.status, ExecutionResultStatus::Succeeded);
        assert!(
            list_payload["processes"]
                .as_array()
                .expect("processes should be array")
                .is_empty()
        );

        let kill = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-kill-owner"),
                tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            owner_context,
            &full_access_policy(),
        );
        assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
    }

    #[test]
    fn diff_preview_reports_text_deltas() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-diff"),
                tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "before": "line1\nsame\nold",
                    "after": "line1\nsame\nnew"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "diff_preview");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(
            payload["preview"]
                .as_str()
                .expect("preview")
                .contains("+new")
        );
        assert!(
            payload["preview"]
                .as_str()
                .expect("preview")
                .contains("-old")
        );
    }

    #[test]
    fn diff_preview_prefers_inline_text_when_path_labels_are_present() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-diff-inline-first"),
                tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "before": "alpha\nbeta",
                    "after": "alpha\nBETA",
                    "before_path": "before",
                    "after_path": "after"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "diff_preview");
        assert!(
            payload["preview"]
                .as_str()
                .expect("preview")
                .contains("+BETA")
        );
    }

    #[test]
    fn builtin_invocation_emits_usage_event_and_updates_ledger() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let missing_path = unique_temp_dir("magi-tool-usage").join("missing.txt");

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-usage"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: missing_path.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);
        let status = event_bus.audit_usage_ledger_status();
        assert_eq!(status.audit_count, 1);
        assert_eq!(status.usage_count, 1);
        let snapshot = event_bus.audit_usage_ledger_snapshot();
        assert_eq!(snapshot.usage_entries.len(), 1);
        assert_eq!(snapshot.usage_entries[0].event_type, "tool.usage.recorded");
        let usage_payload = snapshot.usage_entries[0].payload.clone();
        assert_eq!(usage_payload["tool_name"], "file_read");
        assert_eq!(usage_payload["status"], "Failed");
        assert_eq!(usage_payload["risk_level"], "Low");
    }

    // ── T-204: governance / summary / usage 三者一致性验证 ──────────────────

    #[test]
    fn governance_blocked_invocations_appear_in_summary_and_events() {
        // ShellExec is registered as High risk + Required approval, so default
        // GovernanceService (auto_allow_max_risk=Medium) will block it.
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        // 1) Successful invocation: file.read (Low risk, no approval needed)
        let root = unique_temp_dir("magi-tool-gov-summary");
        let file_path = root.join("ok.txt");
        fs::write(&file_path, "content").expect("write file");

        let ctx = ToolExecutionContext {
            worker_id: Some(WorkerId::new("worker-gov")),
            task_id: Some(TaskId::new("todo-gov")),
            session_id: Some(SessionId::new("session-gov")),
            workspace_id: Some(WorkspaceId::new("workspace-gov")),
            working_directory: None,
        };

        let ok_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-gov-ok"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file_path.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(ok_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(ok_output.governance.outcome, GovernanceOutcome::Allowed);

        // 2) Governance-blocked invocation: shell.exec (High risk)
        let blocked_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-gov-blocked"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf blocked".to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(blocked_output.status, ExecutionResultStatus::NeedsApproval);
        assert_eq!(
            blocked_output.governance.outcome,
            GovernanceOutcome::NeedsApproval
        );

        // 3) Failed invocation: file.read on nonexistent path
        let fail_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-gov-fail"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: root.join("no-such-file.txt").to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(fail_output.status, ExecutionResultStatus::Failed);
        assert_eq!(fail_output.governance.outcome, GovernanceOutcome::Allowed);

        // ── Verify summary reflects all three outcomes ──
        let summary = tool_registry.summary();
        assert_eq!(summary.total_invocations, 3);
        assert_eq!(summary.successful_invocations, 1);
        assert_eq!(summary.blocked_invocations, 1);
        assert_eq!(summary.failed_invocations, 1);

        // ── Verify event bus has matching audit + usage events ──
        let snapshot = event_bus.snapshot();
        let audit_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
            .collect();
        assert_eq!(audit_events.len(), 3);

        let usage_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
            .collect();
        assert_eq!(usage_events.len(), 3);

        // Verify the blocked event carries NeedsApproval status
        let blocked_usage = usage_events
            .iter()
            .find(|e| e.payload["tool_call_id"] == "tc-gov-blocked")
            .expect("blocked usage event");
        assert_eq!(blocked_usage.payload["status"], "NeedsApproval");
        assert_eq!(blocked_usage.payload["risk_level"], "High");

        // Verify the successful event carries Succeeded status
        let ok_usage = usage_events
            .iter()
            .find(|e| e.payload["tool_call_id"] == "tc-gov-ok")
            .expect("ok usage event");
        assert_eq!(ok_usage.payload["status"], "Succeeded");
    }

    #[test]
    fn path_level_write_protection_detects_overlapping_paths() {
        let root = unique_temp_dir("magi-tool-path-conflict");
        let shared_file = root.join("shared.txt");
        fs::write(&shared_file, "data").expect("write shared file");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        // Context A holds a write guard on shared_file via paths
        let ctx_a = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-path-a")),
            session_id: Some(SessionId::new("session-path-a")),
            workspace_id: None,
            working_directory: None,
        };
        let input_a = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-path-a"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf writing",
                "path": shared_file.to_string_lossy(),
                "access_mode": "explicit_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };

        let guard = tool_registry
            .acquire_write_guard(&input_a, &ctx_a, BuiltinToolAccessMode::ExplicitWrite)
            .expect("guard acquisition ok")
            .expect("writeful guard");

        // Context B tries to write to the same path — should conflict
        let ctx_b = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-path-b")),
            session_id: Some(SessionId::new("session-path-a")),
            workspace_id: None,
            working_directory: None,
        };
        let input_b = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-path-b"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf conflict",
                "path": shared_file.to_string_lossy(),
                "access_mode": "explicit_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };
        let blocked_result = tool_registry.acquire_write_guard(
            &input_b,
            &ctx_b,
            BuiltinToolAccessMode::ExplicitWrite,
        );
        assert!(
            blocked_result.is_err(),
            "should be blocked by path-level conflict"
        );
        let err_output = blocked_result.unwrap_err();
        assert_eq!(err_output.status, ExecutionResultStatus::Rejected);
        assert!(err_output.payload.contains("并发写冲突"));

        // After dropping guard A, context B should succeed
        drop(guard);
        let after_result = tool_registry.acquire_write_guard(
            &input_b,
            &ctx_b,
            BuiltinToolAccessMode::ExplicitWrite,
        );
        assert!(after_result.is_ok());
        assert!(after_result.unwrap().is_some());
    }

    #[test]
    fn summary_for_query_filters_by_context_fields() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let root = unique_temp_dir("magi-tool-query-filter");
        let file = root.join("q.txt");
        fs::write(&file, "query").expect("write");

        let ctx_w1 = ToolExecutionContext {
            worker_id: Some(WorkerId::new("w1")),
            task_id: Some(TaskId::new("t1")),
            session_id: Some(SessionId::new("s1")),
            workspace_id: Some(WorkspaceId::new("ws1")),
            working_directory: None,
        };
        let ctx_w2 = ToolExecutionContext {
            worker_id: Some(WorkerId::new("w2")),
            task_id: Some(TaskId::new("t2")),
            session_id: Some(SessionId::new("s1")),
            workspace_id: Some(WorkspaceId::new("ws1")),
            working_directory: None,
        };

        // Execute 2 invocations in context w1
        for i in 0..2 {
            tool_registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new(format!("tc-q-w1-{}", i)),
                    tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: file.to_string_lossy().to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                ctx_w1.clone(),
                &ToolExecutionPolicy::default(),
            );
        }
        // Execute 1 invocation in context w2
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-q-w2-0"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx_w2.clone(),
            &ToolExecutionPolicy::default(),
        );

        // Global summary: 3 total
        let all_summary = tool_registry.summary();
        assert_eq!(all_summary.total_invocations, 3);

        // Query by worker_id=w1: 2
        let w1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            worker_id: Some(WorkerId::new("w1")),
            ..Default::default()
        });
        assert_eq!(w1_summary.total_invocations, 2);
        assert_eq!(w1_summary.successful_invocations, 2);

        // Query by worker_id=w2: 1
        let w2_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            worker_id: Some(WorkerId::new("w2")),
            ..Default::default()
        });
        assert_eq!(w2_summary.total_invocations, 1);

        // Query by task_id=t1: 2
        let t1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            task_id: Some(TaskId::new("t1")),
            ..Default::default()
        });
        assert_eq!(t1_summary.total_invocations, 2);

        // Query by session_id=s1: 3 (shared)
        let s1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            session_id: Some(SessionId::new("s1")),
            ..Default::default()
        });
        assert_eq!(s1_summary.total_invocations, 3);

        // Query by nonexistent worker: 0
        let none_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            worker_id: Some(WorkerId::new("w-nope")),
            ..Default::default()
        });
        assert_eq!(none_summary.total_invocations, 0);
    }

    #[test]
    fn policy_rejection_reflected_in_summary_and_events() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        let root = unique_temp_dir("magi-tool-policy-reject");
        let file = root.join("p.txt");
        fs::write(&file, "policy").expect("write");

        let ctx = ToolExecutionContext::default();

        // Policy that explicitly denies file.read
        let deny_policy = ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::Restricted,
            source_skill_ids: vec!["skill-x".to_string()],
            allowed_tool_names: vec![
                BuiltinToolName::FileRead.as_str().to_string(),
                BuiltinToolName::SearchText.as_str().to_string(),
            ],
            denied_tool_names: vec![BuiltinToolName::FileRead.as_str().to_string()],
        };

        let denied_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-policy-denied"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &deny_policy,
        );
        assert_eq!(denied_output.status, ExecutionResultStatus::Rejected);
        assert_eq!(
            denied_output.governance.outcome,
            GovernanceOutcome::Rejected
        );
        assert_eq!(denied_output.governance.phase, DecisionPhase::ToolPolicy);

        // Policy that only allows search.text — file.read is not in allowed list
        let not_allowed_policy = ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::Restricted,
            source_skill_ids: vec!["skill-y".to_string()],
            allowed_tool_names: vec![BuiltinToolName::SearchText.as_str().to_string()],
            denied_tool_names: vec![],
        };

        let not_allowed_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-policy-not-allowed"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &not_allowed_policy,
        );
        assert_eq!(not_allowed_output.status, ExecutionResultStatus::Rejected);

        // Now do a successful one with default policy
        let ok_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-policy-ok"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(ok_output.status, ExecutionResultStatus::Succeeded);

        // ── Summary: 3 total, 1 success, 2 blocked (policy rejections are Rejected status) ──
        let summary = tool_registry.summary();
        assert_eq!(summary.total_invocations, 3);
        assert_eq!(summary.successful_invocations, 1);
        assert_eq!(summary.blocked_invocations, 2);
        assert_eq!(summary.failed_invocations, 0);

        // ── Events must also carry 3 audit + 3 usage entries ──
        let snapshot = event_bus.snapshot();
        let audit_count = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
            .count();
        assert_eq!(audit_count, 3);

        let usage_count = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
            .count();
        assert_eq!(usage_count, 3);

        // Verify ledger counts match
        let ledger = event_bus.audit_usage_ledger_status();
        assert_eq!(ledger.audit_count, 3);
        assert!(ledger.usage_count >= 3);
    }

    #[test]
    fn full_chain_invocations_events_summary_consistent() {
        // Execute a diverse set of operations and verify every accounting surface agrees.
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(64));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        let root = unique_temp_dir("magi-tool-full-chain");
        let file = root.join("chain.txt");
        fs::write(&file, "chain data").expect("write");

        let ctx = ToolExecutionContext {
            worker_id: Some(WorkerId::new("wk-chain")),
            task_id: Some(TaskId::new("td-chain")),
            session_id: Some(SessionId::new("ss-chain")),
            workspace_id: Some(WorkspaceId::new("ws-chain")),
            working_directory: None,
        };

        // 1) Successful file read
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-1-ok"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 2) Successful diff preview
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-2-diff"),
                tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({"before": "a", "after": "b"}).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 3) Governance-blocked shell exec (high risk)
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-3-blocked"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "rm -rf /".to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 4) Failed file read (nonexistent)
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-4-fail"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: root.join("nonexistent.txt").to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 5) Policy-rejected
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-5-policy"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy {
                access_profile: magi_core::AccessProfile::Restricted,
                source_skill_ids: vec!["sk-locked".to_string()],
                allowed_tool_names: vec![BuiltinToolName::SearchText.as_str().to_string()],
                denied_tool_names: vec![],
            },
        );

        // ── Source 1: invocations list ──
        let invocations = tool_registry.invocations();
        assert_eq!(invocations.len(), 5, "invocations vec has 5 records");

        // ── Source 2: summary ──
        let summary = tool_registry.summary();
        assert_eq!(summary.total_invocations, 5);
        assert_eq!(summary.successful_invocations, 2); // chain-1, chain-2
        assert_eq!(summary.blocked_invocations, 2); // chain-3 (NeedsApproval), chain-5 (Rejected)
        assert_eq!(summary.failed_invocations, 1); // chain-4

        // ── Source 3: event_bus audit events ──
        let snapshot = event_bus.snapshot();
        let audit_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
            .collect();
        assert_eq!(audit_events.len(), 5, "5 audit events");

        // ── Source 4: event_bus usage events ──
        let usage_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
            .collect();
        assert_eq!(usage_events.len(), 5, "5 usage events");

        // ── Cross-check: each invocation has matching audit + usage events ──
        for record in &invocations {
            let call_id = record.tool_call_id.to_string();
            let matching_audit = audit_events
                .iter()
                .find(|e| e.payload["tool_call_id"] == call_id);
            assert!(matching_audit.is_some(), "audit event for {}", call_id);

            let matching_usage = usage_events
                .iter()
                .find(|e| e.payload["tool_call_id"] == call_id);
            assert!(matching_usage.is_some(), "usage event for {}", call_id);

            // Status must agree between invocation record and usage event
            let usage_status = matching_usage.unwrap().payload["status"].as_str().unwrap();
            assert_eq!(
                usage_status,
                format!("{:?}", record.status),
                "status match for {}",
                call_id
            );
        }

        // ── Ledger counts match ──
        let ledger = event_bus.audit_usage_ledger_status();
        assert_eq!(ledger.audit_count, 5);
        assert!(ledger.usage_count >= 5);
    }

    #[test]
    fn full_access_policy_skips_regular_tool_approval() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-full-access-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf full-access".to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy {
                access_profile: magi_core::AccessProfile::FullAccess,
                ..ToolExecutionPolicy::default()
            },
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(output.governance.outcome, GovernanceOutcome::Allowed);
    }

    fn make_registry() -> ToolRegistry {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut r = ToolRegistry::new(governance, event_bus);
        r.register_default_builtins();
        r
    }

    fn full_access_policy() -> ToolExecutionPolicy {
        ToolExecutionPolicy {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionPolicy::default()
        }
    }

    #[test]
    fn registry_enforces_read_only_profile_for_write_tools() {
        let root = unique_temp_dir("magi-tool-read-only-profile");
        let registry = make_registry();
        let target = root.join("blocked.txt");

        let output = registry.execute_with_policy(
            ToolExecutionInput::for_builtin_invocation(
                ToolCallId::new("tc-read-only-file-write"),
                BuiltinToolName::FileWrite.as_str(),
                serde_json::json!({
                    "path": target.to_string_lossy(),
                    "content": "blocked"
                })
                .to_string(),
            ),
            ToolExecutionContext::default(),
            &ToolExecutionPolicy {
                access_profile: magi_core::AccessProfile::ReadOnly,
                ..ToolExecutionPolicy::default()
            },
        );

        assert_eq!(output.status, ExecutionResultStatus::Rejected);
        assert!(!target.exists());
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], BuiltinToolName::FileWrite.as_str());
        assert_eq!(payload["access_profile"], "read_only");
    }

    #[test]
    fn registry_requires_approval_for_restricted_write_shell() {
        let registry = make_registry();

        let output = registry.execute_with_policy(
            ToolExecutionInput::for_builtin_invocation(
                ToolCallId::new("tc-restricted-shell-write"),
                BuiltinToolName::ShellExec.as_str(),
                serde_json::json!({
                    "command": "printf write",
                    "access_mode": "maybe_write"
                })
                .to_string(),
            ),
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::NeedsApproval);
        assert_eq!(output.governance.outcome, GovernanceOutcome::NeedsApproval);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], BuiltinToolName::ShellExec.as_str());
        assert_eq!(payload["access_profile"], "restricted");
    }

    #[test]
    fn registry_allows_restricted_read_only_shell() {
        let registry = make_registry();

        let output = registry.execute_with_policy(
            ToolExecutionInput::for_builtin_invocation(
                ToolCallId::new("tc-restricted-shell-read"),
                BuiltinToolName::ShellExec.as_str(),
                serde_json::json!({
                    "command": "printf hello",
                    "access_mode": "read_only"
                })
                .to_string(),
            ),
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["access_mode"], "read_only");
        assert_eq!(payload["stdout"], "hello");
    }

    fn exec_tool(
        registry: &ToolRegistry,
        tool: BuiltinToolName,
        input: &str,
    ) -> ToolExecutionOutput {
        exec_tool_with_context_and_policy(
            registry,
            tool,
            input,
            ToolExecutionContext::default(),
            ToolExecutionPolicy::default(),
        )
    }

    fn exec_tool_with_context_and_policy(
        registry: &ToolRegistry,
        tool: BuiltinToolName,
        input: &str,
        context: ToolExecutionContext,
        policy: ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new(format!("tc-{}", tool.as_str())),
                tool_name: tool.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: input.to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &policy,
        )
    }

    #[test]
    fn file_write_creates_and_overwrites() {
        let root = unique_temp_dir("magi-tool-file-write");
        let registry = make_registry();
        let file = root.join("new_file.txt");

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "hello world"
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], BuiltinToolName::FileWrite.as_str());
        assert_eq!(payload["created"], true);
        assert_eq!(payload["overwritten"], false);
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "updated"
            })
            .to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        let payload2: Value = serde_json::from_str(&output2.payload).unwrap();
        assert_eq!(payload2["created"], false);
        assert_eq!(payload2["overwritten"], true);
        assert_eq!(fs::read_to_string(&file).unwrap(), "updated");
    }

    #[test]
    fn file_tools_accept_camel_case_path_aliases() {
        let root = unique_temp_dir("magi-tool-file-aliases");
        let registry = make_registry();
        let file = root.join("alias.txt");
        let copy = root.join("alias-copy.txt");
        let moved = root.join("alias-moved.txt");
        let dir = root.join("alias-dir").join("nested");

        let write = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "filePath": file.to_string_lossy(),
                "content": "alias content"
            })
            .to_string(),
        );
        assert_eq!(write.status, ExecutionResultStatus::Succeeded);

        let read = exec_tool(
            &registry,
            BuiltinToolName::FileRead,
            &serde_json::json!({ "filePath": file.to_string_lossy() }).to_string(),
        );
        assert_eq!(read.status, ExecutionResultStatus::Succeeded);

        let mkdir = exec_tool(
            &registry,
            BuiltinToolName::FileMkdir,
            &serde_json::json!({ "dirPath": dir.to_string_lossy() }).to_string(),
        );
        assert_eq!(mkdir.status, ExecutionResultStatus::Succeeded);
        assert!(dir.is_dir());

        let copied = exec_tool(
            &registry,
            BuiltinToolName::FileCopy,
            &serde_json::json!({
                "sourcePath": file.to_string_lossy(),
                "destinationPath": copy.to_string_lossy()
            })
            .to_string(),
        );
        assert_eq!(copied.status, ExecutionResultStatus::Succeeded);
        assert_eq!(fs::read_to_string(&copy).unwrap(), "alias content");

        let moved_output = exec_tool(
            &registry,
            BuiltinToolName::FileMove,
            &serde_json::json!({
                "sourcePath": copy.to_string_lossy(),
                "destinationPath": moved.to_string_lossy()
            })
            .to_string(),
        );
        assert_eq!(moved_output.status, ExecutionResultStatus::Succeeded);
        assert!(!copy.exists());
        assert_eq!(fs::read_to_string(&moved).unwrap(), "alias content");
    }

    #[test]
    fn file_write_creates_parent_dirs() {
        let root = unique_temp_dir("magi-tool-file-write-mkdir");
        let registry = make_registry();
        let file = root.join("a").join("b").join("c.txt");

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "deep"
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(fs::read_to_string(&file).unwrap(), "deep");
    }

    #[test]
    fn file_write_filesystem_failure_uses_public_message() {
        let root = unique_temp_dir("magi-tool-file-write-public-error");
        let registry = make_registry();
        let occupied = root.join("occupied");
        fs::write(&occupied, "not a directory").unwrap();
        let target = occupied.join("child.txt");

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": target.to_string_lossy(),
                "content": "content"
            })
            .to_string(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["error"], "文件暂不可写入，请检查路径或权限");
        let text = output.payload.to_string();
        assert!(!text.contains("occupied"));
        assert!(!text.contains("Not a directory"));
        assert!(!text.contains("os error"));
    }

    #[test]
    fn file_write_rejects_overwrite_when_disabled() {
        let root = unique_temp_dir("magi-tool-file-write-no-overwrite");
        let registry = make_registry();
        let file = root.join("existing.txt");
        fs::write(&file, "original").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "replaced",
                "overwrite": false
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert_eq!(fs::read_to_string(&file).unwrap(), "original");
    }

    #[test]
    fn file_patch_applies_single_replacement() {
        let root = unique_temp_dir("magi-tool-file-patch");
        let registry = make_registry();
        let file = root.join("patch_me.txt");
        fs::write(&file, "line1\nold_value\nline3").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "old_string": "old_value",
                "new_string": "new_value"
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["applied"], 1);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "line1\nnew_value\nline3"
        );
    }

    #[test]
    fn file_patch_empty_patches_falls_back_to_old_new_fields() {
        let root = unique_temp_dir("magi-tool-file-patch-empty-array");
        let registry = make_registry();
        let file = root.join("patch_me.txt");
        fs::write(&file, "alpha needle beta").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "old_string": "needle",
                "new_string": "needle_patched",
                "patches": []
            })
            .to_string(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["applied"], 1);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "alpha needle_patched beta"
        );
    }

    #[test]
    fn file_patch_applies_multiple_patches() {
        let root = unique_temp_dir("magi-tool-file-patch-multi");
        let registry = make_registry();
        let file = root.join("multi.txt");
        fs::write(&file, "aaa\nbbb\nccc").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "patches": [
                    { "old_string": "aaa", "new_string": "AAA" },
                    { "old_string": "ccc", "new_string": "CCC" }
                ]
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["applied"], 2);
        assert_eq!(fs::read_to_string(&file).unwrap(), "AAA\nbbb\nCCC");
    }

    #[test]
    fn file_patch_rejects_ambiguous_match() {
        let root = unique_temp_dir("magi-tool-file-patch-ambig");
        let registry = make_registry();
        let file = root.join("dup.txt");
        fs::write(&file, "same\nsame\nother").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "old_string": "same",
                "new_string": "replaced"
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert_eq!(fs::read_to_string(&file).unwrap(), "same\nsame\nother");
    }

    #[test]
    fn apply_patch_tool_applies_patch_envelope_through_registry() {
        let root = unique_temp_dir("magi-tool-apply-patch");
        let registry = make_registry();
        fs::write(root.join("existing.txt"), "alpha\nbeta\n").unwrap();

        let input = serde_json::json!({
            "patch": "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Update File: existing.txt\n@@\n-alpha\n+ALPHA\n beta\n*** End Patch\n"
        })
        .to_string();
        let output = registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-apply-patch"),
                tool_name: BuiltinToolName::ApplyPatch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input,
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Medium,
            },
            ToolExecutionContext {
                working_directory: Some(root.clone()),
                ..ToolExecutionContext::default()
            },
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], "apply_patch");
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["operations"], 2);
        assert_eq!(
            fs::read_to_string(root.join("created.txt")).unwrap(),
            "created\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("existing.txt")).unwrap(),
            "ALPHA\nbeta\n"
        );
    }

    #[test]
    fn file_remove_deletes_file_and_directory() {
        let root = unique_temp_dir("magi-tool-file-remove");
        let registry = make_registry();
        let file = root.join("del_me.txt");
        fs::write(&file, "bye").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileRemove,
            &serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert!(!file.exists());

        let subdir = root.join("nested");
        fs::create_dir_all(subdir.join("child")).unwrap();
        fs::write(subdir.join("child").join("f.txt"), "x").unwrap();

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileRemove,
            &serde_json::json!({ "path": subdir.to_string_lossy(), "recursive": true }).to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        assert!(!subdir.exists());
    }

    #[test]
    fn file_remove_rejects_workspace_root_even_in_full_access() {
        let root = unique_temp_dir("magi-tool-file-remove-protected-root");
        fs::write(root.join("keep.txt"), "keep").unwrap();
        let registry = make_registry();

        let output = exec_tool_with_context_and_policy(
            &registry,
            BuiltinToolName::FileRemove,
            &serde_json::json!({ "path": ".", "recursive": true }).to_string(),
            ToolExecutionContext {
                working_directory: Some(root.clone()),
                ..ToolExecutionContext::default()
            },
            ToolExecutionPolicy {
                access_profile: magi_core::AccessProfile::FullAccess,
                ..ToolExecutionPolicy::default()
            },
        );

        assert_eq!(output.status, ExecutionResultStatus::Rejected);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("拒绝删除受保护路径")
        );
        assert!(root.join("keep.txt").exists());
    }

    #[test]
    fn file_remove_rejects_absolute_working_directory_even_in_full_access() {
        let root = unique_temp_dir("magi-tool-file-remove-protected-absolute");
        fs::write(root.join("keep.txt"), "keep").unwrap();
        let registry = make_registry();

        let output = exec_tool_with_context_and_policy(
            &registry,
            BuiltinToolName::FileRemove,
            &serde_json::json!({ "path": root.to_string_lossy(), "recursive": true }).to_string(),
            ToolExecutionContext {
                working_directory: Some(root.clone()),
                ..ToolExecutionContext::default()
            },
            ToolExecutionPolicy {
                access_profile: magi_core::AccessProfile::FullAccess,
                ..ToolExecutionPolicy::default()
            },
        );

        assert_eq!(output.status, ExecutionResultStatus::Rejected);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("拒绝删除当前工作目录")
        );
        assert!(root.join("keep.txt").exists());
    }

    #[test]
    fn file_remove_rejects_filesystem_root_even_in_full_access() {
        let registry = make_registry();

        let output = exec_tool_with_context_and_policy(
            &registry,
            BuiltinToolName::FileRemove,
            &serde_json::json!({ "path": "/", "recursive": true }).to_string(),
            ToolExecutionContext::default(),
            ToolExecutionPolicy {
                access_profile: magi_core::AccessProfile::FullAccess,
                ..ToolExecutionPolicy::default()
            },
        );

        assert_eq!(output.status, ExecutionResultStatus::Rejected);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("拒绝删除受保护路径")
        );
    }

    #[test]
    fn file_mkdir_creates_nested_dirs() {
        let root = unique_temp_dir("magi-tool-file-mkdir");
        let registry = make_registry();
        let deep = root.join("x").join("y").join("z");

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileMkdir,
            &serde_json::json!({ "path": deep.to_string_lossy() }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert!(deep.is_dir());

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileMkdir,
            &serde_json::json!({ "path": deep.to_string_lossy() }).to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output2.payload).unwrap();
        assert_eq!(payload["already_existed"], true);
    }

    #[test]
    fn file_copy_copies_file_and_directory() {
        let root = unique_temp_dir("magi-tool-file-copy");
        let registry = make_registry();

        let src = root.join("src.txt");
        let dst = root.join("dst.txt");
        fs::write(&src, "copy me").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileCopy,
            &serde_json::json!({
                "source": src.to_string_lossy(),
                "destination": dst.to_string_lossy()
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(fs::read_to_string(&dst).unwrap(), "copy me");
        assert!(src.exists());

        let src_dir = root.join("src_dir");
        fs::create_dir_all(src_dir.join("sub")).unwrap();
        fs::write(src_dir.join("sub").join("f.txt"), "nested").unwrap();
        let dst_dir = root.join("dst_dir");

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileCopy,
            &serde_json::json!({
                "source": src_dir.to_string_lossy(),
                "destination": dst_dir.to_string_lossy()
            })
            .to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            fs::read_to_string(dst_dir.join("sub").join("f.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn file_move_renames_file() {
        let root = unique_temp_dir("magi-tool-file-move");
        let registry = make_registry();

        let src = root.join("old.txt");
        let dst = root.join("new.txt");
        fs::write(&src, "move me").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileMove,
            &serde_json::json!({
                "source": src.to_string_lossy(),
                "destination": dst.to_string_lossy()
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "move me");
    }

    #[test]
    fn file_move_rejects_existing_destination_without_overwrite() {
        let root = unique_temp_dir("magi-tool-file-move-no-overwrite");
        let registry = make_registry();

        let src = root.join("a.txt");
        let dst = root.join("b.txt");
        fs::write(&src, "from").unwrap();
        fs::write(&dst, "existing").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileMove,
            &serde_json::json!({
                "source": src.to_string_lossy(),
                "destination": dst.to_string_lossy()
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(src.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "existing");
    }

    // ── from_str 映射 + helper 方法 ──

    #[test]
    fn from_str_handles_all_canonical_names() {
        for tool in all_builtin_tools() {
            assert_eq!(
                BuiltinToolName::from_str(tool.as_str()),
                Some(tool),
                "canonical name {} should resolve",
                tool.as_str()
            );
        }
    }

    #[test]
    fn from_str_handles_ts_compatible_aliases() {
        let aliases = [
            ("file_view", BuiltinToolName::FileRead),
            ("image_view", BuiltinToolName::ViewImage),
            ("file_create", BuiltinToolName::FileWrite),
            ("file_edit", BuiltinToolName::FilePatch),
            ("file_insert", BuiltinToolName::FilePatch),
            ("file_remove", BuiltinToolName::FileRemove),
            ("code_search_regex", BuiltinToolName::SearchText),
            ("code_search_semantic", BuiltinToolName::SearchSemantic),
            ("web_search", BuiltinToolName::WebSearch),
            ("web_fetch", BuiltinToolName::WebFetch),
            ("project_knowledge_query", BuiltinToolName::KnowledgeQuery),
            ("tool_diagnostics", BuiltinToolName::ToolCatalog),
        ];
        for (alias, expected) in &aliases {
            assert_eq!(
                BuiltinToolName::from_str(alias),
                Some(*expected),
                "TS alias {} should resolve",
                alias
            );
        }
        assert_eq!(BuiltinToolName::from_str("nonexistent_tool"), None);
        assert_eq!(BuiltinToolName::from_str("mermaid_diagram"), None);
    }

    #[test]
    fn from_str_roundtrips_through_as_str() {
        for tool in all_builtin_tools() {
            assert_eq!(
                BuiltinToolName::from_str(tool.as_str()),
                Some(tool),
                "{:?} roundtrip failed",
                tool
            );
        }
    }

    #[test]
    fn canonical_builtin_tool_name_uses_builtin_aliases() {
        assert_eq!(
            canonical_builtin_tool_name("file_edit"),
            Some("file_patch".to_string())
        );
        assert_eq!(
            canonical_builtin_tool_name("tool_diagnostics"),
            Some("tool_catalog".to_string())
        );
        assert_eq!(canonical_builtin_tool_name("unknown_tool"), None);
    }

    #[test]
    fn is_write_operation_identifies_correct_tools() {
        let write_ops = [
            BuiltinToolName::FileWrite,
            BuiltinToolName::FilePatch,
            BuiltinToolName::ApplyPatch,
            BuiltinToolName::FileRemove,
            BuiltinToolName::FileMkdir,
            BuiltinToolName::FileCopy,
            BuiltinToolName::FileMove,
            BuiltinToolName::AgentSpawn,
            BuiltinToolName::TodoWrite,
            BuiltinToolName::MemoryWrite,
            BuiltinToolName::MissionCharterWrite,
            BuiltinToolName::PlanWrite,
            BuiltinToolName::KgWrite,
            BuiltinToolName::ValidationRecord,
            BuiltinToolName::Checkpoint,
            BuiltinToolName::HumanCheckpointRequest,
        ];
        let non_write = [
            BuiltinToolName::FileRead,
            BuiltinToolName::ViewImage,
            BuiltinToolName::SearchText,
            BuiltinToolName::ShellExec,
            BuiltinToolName::AgentWait,
            BuiltinToolName::WebSearch,
            BuiltinToolName::DiffPreview,
            BuiltinToolName::DiagramRender,
            BuiltinToolName::ToolCatalog,
        ];
        for tool in &write_ops {
            assert!(tool.is_write_operation(), "{:?} should be write", tool);
        }
        for tool in &non_write {
            assert!(!tool.is_write_operation(), "{:?} should not be write", tool);
        }
    }

    #[test]
    fn builtin_permission_engine_uses_builtin_access_modes() {
        let engine = builtin_permission_engine();
        let policy = magi_permissions::PermissionPolicy::default();

        let patch_request = magi_permissions::PermissionRequest::ToolInvocation {
            tool_name: BuiltinToolName::FilePatch.as_str(),
            is_write_tool: true,
        };
        assert_eq!(
            engine.decide(
                &patch_request,
                &policy,
                magi_core::AccessProfile::Restricted
            ),
            magi_permissions::Decision::Allow
        );

        let memory_write_request = magi_permissions::PermissionRequest::ToolInvocation {
            tool_name: BuiltinToolName::MemoryWrite.as_str(),
            is_write_tool: true,
        };
        assert_eq!(
            engine.decide(
                &memory_write_request,
                &policy,
                magi_core::AccessProfile::Restricted
            ),
            magi_permissions::Decision::Allow
        );
        assert!(matches!(
            engine.decide(
                &memory_write_request,
                &policy,
                magi_core::AccessProfile::ReadOnly
            ),
            magi_permissions::Decision::Deny { .. }
        ));

        let shell_request = magi_permissions::PermissionRequest::ToolInvocation {
            tool_name: BuiltinToolName::ShellExec.as_str(),
            is_write_tool: true,
        };
        assert!(matches!(
            engine.decide(
                &shell_request,
                &policy,
                magi_core::AccessProfile::Restricted
            ),
            magi_permissions::Decision::NeedsApproval { .. }
        ));

        assert!(engine.is_read_only_tool(BuiltinToolName::ToolCatalog.as_str()));
        assert!(!engine.is_read_only_tool("tool_diagnostics"));
    }

    // ── diagram.render 验证 ──

    #[test]
    fn diagram_render_schema_guides_mind_maps_to_structured_payload() {
        let schema = BuiltinToolName::DiagramRender.parameters_schema();
        let kind_description = schema["properties"]["kind"]["description"]
            .as_str()
            .unwrap_or_default();
        let source_description = schema["properties"]["source"]["description"]
            .as_str()
            .unwrap_or_default();
        let graph_description = schema["properties"]["graph"]["description"]
            .as_str()
            .unwrap_or_default();

        assert!(kind_description.contains("思维导图"));
        assert!(kind_description.contains("不要使用 Mermaid mindmap"));
        assert!(source_description.contains("不支持 Mermaid mindmap"));
        assert!(graph_description.contains("中心主题"));
    }

    #[test]
    fn diagram_render_recognizes_mermaid_types() {
        let registry = make_registry();
        let valid_codes = [
            ("graph TD\n  A --> B", "flowchart"),
            ("flowchart LR\n  A --> B", "flowchart"),
            (
                "---\nconfig:\n  layout: elk\n---\nflowchart LR\n  A --> B",
                "flowchart",
            ),
            ("sequenceDiagram\n  A->>B: Hello", "sequence"),
            ("classDiagram\n  class A", "class"),
            ("stateDiagram-v2\n  [*] --> S", "state"),
            ("erDiagram\n  A ||--o{ B : has", "er"),
            ("gantt\n  title Plan", "gantt"),
            ("pie\n  title Usage", "pie"),
            ("gitGraph\n  commit", "git"),
            ("timeline\n  2024", "timeline"),
        ];
        for (code, expected_type) in &valid_codes {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &serde_json::json!({ "kind": "mermaid", "source": code, "layout": "elk" })
                    .to_string(),
            );
            assert_eq!(
                output.status,
                ExecutionResultStatus::Succeeded,
                "code: {}",
                code
            );
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert_eq!(payload["tool"], "diagram_render");
            assert_eq!(payload["type"], "diagram_render");
            assert_eq!(payload["kind"], "mermaid");
            assert_eq!(payload["layout"], "elk");
            assert_eq!(payload["diagram_type"], *expected_type, "code: {}", code);
        }
    }

    #[test]
    fn diagram_render_accepts_dot_graph_and_flow_kinds() {
        let registry = make_registry();

        let dot = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": "dot",
                "source": "digraph G { A -> B }",
                "title": "DOT"
            })
            .to_string(),
        );
        assert_eq!(dot.status, ExecutionResultStatus::Succeeded);
        let dot_payload: Value = serde_json::from_str(&dot.payload).unwrap();
        assert_eq!(dot_payload["kind"], "dot");
        assert_eq!(dot_payload["diagram_type"], "dot");

        for kind in ["graph", "flow"] {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &serde_json::json!({
                    "kind": kind,
                    "layout": "cose",
                    "graph": {
                        "nodes": [
                            { "id": "a", "label": "A" },
                            { "id": "b", "label": "B" }
                        ],
                        "edges": [
                            { "source": "a", "target": "b", "label": "relates" }
                        ]
                    }
                })
                .to_string(),
            );
            assert_eq!(output.status, ExecutionResultStatus::Succeeded, "{kind}");
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert_eq!(payload["kind"], kind);
            assert_eq!(payload["layout"], "cose");
            assert_eq!(payload["interactive"], true);
            assert_eq!(payload["graph"]["nodes"].as_array().unwrap().len(), 2);
        }

        for layout in ["fcose", "cose-bilkent"] {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &serde_json::json!({
                    "kind": "graph",
                    "layout": layout,
                    "graph": {
                        "nodes": [
                            { "id": "a", "label": "A" },
                            { "id": "b", "label": "B" }
                        ],
                        "edges": [
                            { "source": "a", "target": "b" }
                        ]
                    }
                })
                .to_string(),
            );
            assert_eq!(output.status, ExecutionResultStatus::Succeeded, "{layout}");
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert_eq!(payload["layout"], layout);
        }
    }

    #[test]
    fn diagram_render_rejects_invalid_inputs() {
        let registry = make_registry();
        for input in [
            serde_json::json!({ "kind": "mermaid", "source": "invalid_diagram\n  A --> B" }),
            serde_json::json!({ "kind": "mermaid", "source": "mindmap\n  root\n    child" }),
            serde_json::json!({ "kind": "mermaid", "source": "  " }),
            serde_json::json!({ "kind": "dot", "source": "A -> B" }),
            serde_json::json!({ "kind": "graph", "graph": { "nodes": [] } }),
            serde_json::json!({ "kind": "cytoscape", "graph": { "nodes": [], "edges": [] } }),
        ] {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &input.to_string(),
            );
            assert_eq!(output.status, ExecutionResultStatus::Failed, "{input}");
        }
    }

    #[test]
    fn diagram_render_requires_structured_payload_for_mind_maps() {
        let registry = make_registry();

        let mermaid_mindmap = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": "mermaid",
                "source": "mindmap\n  root((验证自动保存规则))\n    目标\n      确认输出结果"
            })
            .to_string(),
        );
        assert_eq!(mermaid_mindmap.status, ExecutionResultStatus::Failed);
        let failed_payload: Value = serde_json::from_str(&mermaid_mindmap.payload).unwrap();
        assert!(
            failed_payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("kind=flow 或 kind=graph"),
            "{failed_payload}"
        );

        let flow_mindmap = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": "flow",
                "graph": {
                    "nodes": [
                        { "id": "root", "label": "验证自动保存规则" },
                        { "id": "goal", "label": "目标" },
                        { "id": "result", "label": "确认输出结果" }
                    ],
                    "edges": [
                        { "source": "root", "target": "goal" },
                        { "source": "goal", "target": "result" }
                    ]
                }
            })
            .to_string(),
        );
        assert_eq!(flow_mindmap.status, ExecutionResultStatus::Succeeded);
        let flow_payload: Value = serde_json::from_str(&flow_mindmap.payload).unwrap();
        assert_eq!(flow_payload["kind"], "flow");
        assert_eq!(flow_payload["interactive"], true);
    }

    // ── 实际工具行为验证 ──

    #[test]
    fn search_semantic_requires_workspace_index() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::SearchSemantic,
            &serde_json::json!({ "query": "test query" }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], "search_semantic");
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error"], "代码索引引擎不可用");
    }

    #[test]
    fn knowledge_query_reads_workspace_knowledge_store() {
        let store = Arc::new(magi_knowledge_store::KnowledgeStore::new());
        let workspace_id = WorkspaceId::new("workspace-knowledge-query");
        store.upsert(magi_knowledge_store::KnowledgeRecord {
            knowledge_id: "kb-runtime-architecture".to_string(),
            kind: magi_knowledge_store::KnowledgeKind::Learning,
            title: "Runtime architecture".to_string(),
            content: "The runtime architecture keeps knowledge in the governed workspace store."
                .to_string(),
            tags: vec!["runtime".to_string()],
            workspace_id: Some(workspace_id.clone()),
            source_ref: Some("memory/runtime.md".to_string()),
            updated_at: UtcMillis(100),
        });
        store.upsert(magi_knowledge_store::KnowledgeRecord {
            knowledge_id: "kb-other-workspace".to_string(),
            kind: magi_knowledge_store::KnowledgeKind::Learning,
            title: "Other workspace".to_string(),
            content: "The same architecture term must not leak across workspaces.".to_string(),
            tags: vec!["runtime".to_string()],
            workspace_id: Some(WorkspaceId::new("workspace-knowledge-query-other")),
            source_ref: Some("memory/other.md".to_string()),
            updated_at: UtcMillis(200),
        });

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut registry = ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
        registry.register_default_builtins();

        let output = registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-knowledge-query"),
                tool_name: BuiltinToolName::KnowledgeQuery.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "query": "runtime architecture",
                    "kind": "learning",
                    "tags": ["runtime"],
                    "limit": 5
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext {
                workspace_id: Some(workspace_id.clone()),
                ..ToolExecutionContext::default()
            },
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], "knowledge_query");
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["workspace_id"], workspace_id.as_str());
        assert_eq!(payload["kind"], "learning");
        assert_eq!(payload["total_matches"], 1);
        assert_eq!(payload["returned_matches"], 1);
        assert_eq!(
            payload["results"][0]["knowledge_id"],
            "kb-runtime-architecture"
        );
        assert_eq!(payload["results"][0]["source_ref"], "memory/runtime.md");
    }

    #[test]
    fn skill_apply_is_not_registered_as_builtin() {
        let registry = make_registry();
        assert!(registry.builtin_access_mode("skill_apply").is_none());
        assert!(
            registry
                .builtin_specs()
                .iter()
                .all(|spec| spec.name != "skill_apply")
        );
    }

    #[test]
    fn orchestration_tools_are_not_registered_as_builtins() {
        let registry = make_registry();
        for tool_name in [
            "task_split",
            "task_list",
            "task_update",
            "task_claim_next",
            "context_compact",
        ] {
            assert!(
                BuiltinToolName::from_str(tool_name).is_none(),
                "{tool_name}"
            );
            assert!(
                registry.builtin_access_mode(tool_name).is_none(),
                "{tool_name}"
            );
            assert!(
                registry
                    .builtin_specs()
                    .iter()
                    .all(|spec| spec.name != tool_name),
                "{tool_name}"
            );
        }
    }

    // ── web 工具 access mode ──

    #[test]
    fn web_tools_are_read_only() {
        let registry = make_registry();
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::ViewImage.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::WebSearch.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::WebFetch.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::DiagramRender.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::ToolCatalog.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
    }

    #[test]
    fn web_fetch_reads_local_http_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("local test server address")
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept web_fetch request");
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let body = r#"<!doctype html><html><body><main><h1>Smoke Web Fetch</h1><p>alpha beta</p></main></body></html>"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write web_fetch response");
        });

        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::WebFetch,
            &serde_json::json!({ "url": url }).to_string(),
        );
        server.join().expect("local web_fetch server should finish");

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], BuiltinToolName::WebFetch.as_str());
        assert_eq!(payload["status"], "succeeded");
        assert!(
            payload["content"]
                .as_str()
                .expect("content should be string")
                .contains("Smoke Web Fetch")
        );
    }

    #[test]
    #[ignore = "live network smoke for manually verifying DuckDuckGo-backed web_search"]
    fn web_search_live_smoke_returns_json_payload() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::WebSearch,
            &serde_json::json!({ "query": "OpenAI" }).to_string(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], BuiltinToolName::WebSearch.as_str());
        assert_eq!(payload["status"], "succeeded");
        assert!(payload["result_count"].is_number());
        assert!(payload["results"].is_array());
    }

    // ── 默认内置工具全覆盖注册验证 ──

    #[test]
    fn all_default_builtins_are_registered() {
        let registry = make_registry();
        let specs = registry.builtin_specs();
        let all_tools = all_builtin_tools();
        assert_eq!(specs.len(), all_tools.len(), "应注册全部默认内置工具");
        for tool in &all_tools {
            assert!(
                registry.builtin_access_mode(tool.as_str()).is_some(),
                "{:?} should be registered",
                tool
            );
        }
    }

    #[test]
    fn public_builtin_specs_exclude_shell_internal_process_tools() {
        let registry = make_registry();
        let public_specs = registry.public_builtin_specs();
        let public_names: Vec<_> = public_specs.iter().map(|spec| spec.name.as_str()).collect();

        assert_eq!(
            public_names,
            vec![
                "file_read",
                "view_image",
                "file_write",
                "file_patch",
                "apply_patch",
                "file_remove",
                "file_mkdir",
                "file_copy",
                "file_move",
                "search_text",
                "search_semantic",
                "shell_exec",
                "process_inspect",
                "diff_preview",
                "web_search",
                "web_fetch",
                "diagram_render",
                "knowledge_query",
                "code_symbols",
                "tool_catalog",
                "agent_spawn",
                "agent_wait",
                "todo_write",
                "memory_write",
                "mission_charter_write",
                "plan_write",
                "kg_write",
                "validation_record",
                "checkpoint_create",
                "human_checkpoint_request",
            ],
            "public builtin specs must remain the single canonical tool surface"
        );

        assert!(is_public_builtin_tool_surface("shell_exec"));
        assert!(!is_public_builtin_tool_surface("process_launch"));
        assert!(!is_public_builtin_tool_surface("process_read"));
        assert!(!is_public_builtin_tool_surface("process_write"));
        assert!(!is_public_builtin_tool_surface("process_kill"));
        assert!(!is_public_builtin_tool_surface("process_list"));
        assert!(is_public_builtin_tool_surface("process_inspect"));
        assert!(
            public_specs
                .iter()
                .any(|spec| spec.name == BuiltinToolName::ShellExec.as_str())
        );
        for internal_tool in [
            BuiltinToolName::ProcessLaunch,
            BuiltinToolName::ProcessRead,
            BuiltinToolName::ProcessWrite,
            BuiltinToolName::ProcessKill,
            BuiltinToolName::ProcessList,
        ] {
            assert!(
                public_specs
                    .iter()
                    .all(|spec| spec.name != internal_tool.as_str())
            );
        }
    }

    #[test]
    fn public_object_schemas_always_define_required_array() {
        for tool in BuiltinToolName::ALL {
            if !tool.is_public_tool_surface() {
                continue;
            }
            let schema = tool.parameters_schema();
            if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
                continue;
            }
            assert!(
                schema
                    .get("required")
                    .is_some_and(serde_json::Value::is_array),
                "{} schema must define required as an array for OpenAI-compatible providers",
                tool.as_str()
            );
        }
    }

    #[test]
    fn diagram_renderer_names_are_not_builtin_tools() {
        for name in [
            "mermaid_diagram",
            "mermaid",
            "graphviz",
            "dot",
            "cytoscape",
            "svelte_flow",
            "svelte-flow",
        ] {
            assert_eq!(
                BuiltinToolName::from_str(name),
                None,
                "{name} must stay a renderer/kind behind diagram_render, not a builtin tool"
            );
            assert!(
                !is_public_builtin_tool_surface(name),
                "{name} must not be accepted as a public builtin surface"
            );
        }
    }

    #[test]
    fn builtin_invocation_policy_classifies_shell_exec_by_runtime_intent() {
        let read_only = BuiltinToolName::ShellExec.invocation_policy_for_input(
            &serde_json::json!({
                "command": "git status --short",
                "access_mode": "read_only"
            })
            .to_string(),
        );
        assert_eq!(read_only.risk_level, RiskLevel::Low);
        assert_eq!(read_only.approval_requirement, ApprovalRequirement::None);

        let misdeclared_read_only = BuiltinToolName::ShellExec.invocation_policy_for_input(
            &serde_json::json!({
                "command": "printf hidden > out.txt",
                "access_mode": "read_only"
            })
            .to_string(),
        );
        assert_eq!(misdeclared_read_only.risk_level, RiskLevel::Medium);
        assert_eq!(
            misdeclared_read_only.approval_requirement,
            ApprovalRequirement::None
        );

        let background = BuiltinToolName::ShellExec.invocation_policy_for_input(
            &serde_json::json!({
                "command": "cargo check",
                "background": true
            })
            .to_string(),
        );
        assert_eq!(background.risk_level, RiskLevel::Medium);
        assert_eq!(background.approval_requirement, ApprovalRequirement::None);

        let background_read = BuiltinToolName::ShellExec.invocation_policy_for_input(
            &serde_json::json!({
                "action": "read",
                "terminal_id": 1
            })
            .to_string(),
        );
        assert_eq!(background_read.risk_level, RiskLevel::Low);
        assert_eq!(
            background_read.approval_requirement,
            ApprovalRequirement::None
        );

        let raw_shell = BuiltinToolName::ShellExec.invocation_policy_for_input("cargo test");
        assert_eq!(raw_shell.risk_level, RiskLevel::Medium);
        assert_eq!(raw_shell.approval_requirement, ApprovalRequirement::None);
    }

    #[test]
    fn builtin_invocation_policy_requires_approval_for_recursive_remove() {
        let single_file = BuiltinToolName::FileRemove
            .invocation_policy_for_input(&serde_json::json!({ "path": "tmp.txt" }).to_string());
        assert_eq!(single_file.risk_level, RiskLevel::Medium);
        assert_eq!(single_file.approval_requirement, ApprovalRequirement::None);

        let recursive = BuiltinToolName::FileRemove.invocation_policy_for_input(
            &serde_json::json!({ "path": "target/tmp", "recursive": true }).to_string(),
        );
        assert_eq!(recursive.risk_level, RiskLevel::High);
        assert_eq!(
            recursive.approval_requirement,
            ApprovalRequirement::Required
        );
    }

    #[test]
    fn builtin_execution_input_canonicalizes_alias_and_applies_invocation_policy() {
        let file_view = ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-file-view"),
            "file_view",
            "/tmp/example.txt",
        );
        assert_eq!(file_view.tool_name, BuiltinToolName::FileRead.as_str());
        assert_eq!(file_view.risk_level, RiskLevel::Low);
        assert_eq!(file_view.approval_requirement, ApprovalRequirement::None);

        let recursive_remove = ToolExecutionInput::for_builtin_invocation(
            ToolCallId::new("tool-call-file-remove"),
            "file_remove",
            serde_json::json!({ "path": "target/tmp", "recursive": true }).to_string(),
        );
        assert_eq!(
            recursive_remove.tool_name,
            BuiltinToolName::FileRemove.as_str()
        );
        assert_eq!(recursive_remove.risk_level, RiskLevel::High);
        assert_eq!(
            recursive_remove.approval_requirement,
            ApprovalRequirement::Required
        );
    }

    #[test]
    fn registry_rejects_internal_process_tools_as_public_builtin_calls() {
        let registry = make_registry();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-internal-process")),
            session_id: Some(SessionId::new("session-internal-process")),
            workspace_id: Some(WorkspaceId::new("workspace-internal-process")),
            working_directory: None,
        };

        for (tool_name, input) in [
            (
                BuiltinToolName::ProcessLaunch.as_str(),
                serde_json::json!({ "command": "sleep 1" }),
            ),
            (
                BuiltinToolName::ProcessRead.as_str(),
                serde_json::json!({ "terminal_id": 1 }),
            ),
            (
                BuiltinToolName::ProcessWrite.as_str(),
                serde_json::json!({ "terminal_id": 1, "input": "x" }),
            ),
            (
                BuiltinToolName::ProcessKill.as_str(),
                serde_json::json!({ "terminal_id": 1 }),
            ),
            (BuiltinToolName::ProcessList.as_str(), serde_json::json!({})),
        ] {
            let output = registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new(format!("tool-call-{tool_name}-internal")),
                    tool_name: tool_name.to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: input.to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                context.clone(),
                &ToolExecutionPolicy::default(),
            );

            assert_eq!(
                output.status,
                ExecutionResultStatus::Rejected,
                "{tool_name} must be rejected before internal process execution"
            );
            assert!(
                output.payload.contains("shell_exec"),
                "{tool_name} rejection should point callers to shell_exec, got {}",
                output.payload
            );
        }
    }

    #[test]
    fn shell_exec_background_keeps_shell_public_payload() {
        let registry = make_registry();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-shell-background")),
            session_id: Some(SessionId::new("session-shell-background")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-background")),
            working_directory: None,
        };

        let output = registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-background"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf shell-background-ok",
                    "background": true
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &full_access_policy(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload should parse");
        assert_eq!(payload["tool"], "shell_exec");
        assert_eq!(payload["mode"], "background");
        assert!(payload["terminal_id"].as_u64().is_some());
    }

    #[test]
    fn file_write_execution_respects_active_write_guard() {
        let registry = make_registry();
        let root = unique_temp_dir("magi-tool-file-write-guard");
        let file = root.join("guarded.txt");
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-file-write-guard")),
            session_id: Some(SessionId::new("session-file-write-guard")),
            workspace_id: Some(WorkspaceId::new("workspace-file-write-guard")),
            working_directory: None,
        };
        let held_input = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-file-write-held"),
            tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "held"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };
        let _held_guard = registry
            .acquire_write_guard(&held_input, &context, BuiltinToolAccessMode::ExplicitWrite)
            .expect("held write guard should acquire");

        let output = registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-file-write-conflict"),
                tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "path": file.to_string_lossy(),
                    "content": "conflict"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Rejected);
        assert!(
            output.payload.contains("并发写冲突"),
            "file_write should be protected by the write guard, got {}",
            output.payload
        );
    }

    #[test]
    fn builtin_access_mode_reports_write_tools_correctly() {
        let registry = make_registry();
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileRead.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileWrite.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FilePatch.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::ApplyPatch.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileRemove.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileMkdir.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileCopy.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileMove.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::AgentSpawn.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::TodoWrite.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::MemoryWrite.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::PlanWrite.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::KgWrite.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::HumanCheckpointRequest.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::ShellExec.as_str()),
            Some(BuiltinToolAccessMode::MaybeWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::ProcessWrite.as_str()),
            Some(BuiltinToolAccessMode::MaybeWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::ProcessKill.as_str()),
            Some(BuiltinToolAccessMode::MaybeWrite)
        );
    }

    #[test]
    fn search_semantic_uses_workspace_local_index() {
        // 造一个含已知符号的小仓库，验证 search_semantic 只走工作区本地代码索引。
        let root = unique_temp_dir("magi-tool-search-fuse");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("src/auth.rs"),
            "pub fn authenticate_user(token: &str) -> bool { !token.is_empty() }\n",
        )
        .expect("write auth.rs");

        // 构建索引并注入 KnowledgeStore。
        let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
        let workspace_id = WorkspaceId::new("workspace-search-fuse");
        store.build_workspace_index(&workspace_id, &root);

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry =
            ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
        tool_registry.register_default_builtins();

        let context = ToolExecutionContext {
            workspace_id: Some(workspace_id),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-search-fuse"),
                tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "query": "authenticate user" }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["engine"], "local_search_engine");
        assert_eq!(payload["workspace_id"], "workspace-search-fuse");
        let results = payload["results"].as_array().expect("results array");
        assert!(!results.is_empty(), "应有命中结果");
        assert!(
            results.iter().any(|r| r["source"] == "engine"
                && r["path"].as_str().is_some_and(|p| p.contains("auth.rs"))),
            "本地索引应命中 auth.rs，实际: {results:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_semantic_does_not_fallback_to_text_scan() {
        let root = unique_temp_dir("magi-tool-search-no-scan");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src/main.rs"), "pub fn unrelated_code() {}\n").expect("write main.rs");
        fs::write(
            root.join("notes.txt"),
            "only_in_txt_note should not be returned by code index search\n",
        )
        .expect("write notes.txt");

        let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
        let workspace_id = WorkspaceId::new("workspace-search-no-scan");
        store.build_workspace_index(&workspace_id, &root);

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry =
            ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
        tool_registry.register_default_builtins();

        let context = ToolExecutionContext {
            workspace_id: Some(workspace_id),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-search-no-scan"),
                tool_name: BuiltinToolName::SearchSemantic.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "query": "only_in_txt_note" }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["engine"], "local_search_engine");
        assert_eq!(payload["returned_matches"], 0);
        assert!(
            payload["results"]
                .as_array()
                .expect("results array")
                .is_empty(),
            "非代码文件不应通过旧文本扫描兜底命中"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_symbols_definition_and_file_symbols() {
        let root = unique_temp_dir("magi-tool-code-symbols");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("src/auth.rs"),
            "pub fn authenticate_user(token: &str) -> bool { !token.is_empty() }\n\
             struct Session { id: u32 }\n",
        )
        .expect("write auth.rs");

        let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
        let workspace_id = WorkspaceId::new("workspace-code-symbols");
        store.build_workspace_index(&workspace_id, &root);

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry =
            ToolRegistry::new(governance, event_bus).with_knowledge_store(store);
        tool_registry.register_default_builtins();

        let context = ToolExecutionContext {
            workspace_id: Some(workspace_id),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        // definition：按名查定义
        let def = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-def"),
                tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "action": "definition", "name": "authenticate_user" })
                    .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(def.status, ExecutionResultStatus::Succeeded);
        let def_payload: Value = serde_json::from_str(&def.payload).expect("def json");
        let def_results = def_payload["results"].as_array().expect("def results");
        assert!(
            def_results.iter().any(|r| r["name"] == "authenticate_user"
                && r["path"].as_str().is_some_and(|p| p.contains("auth.rs"))),
            "definition 应命中 authenticate_user@auth.rs，实际: {def_results:?}"
        );

        // file_symbols：列文件符号
        let list = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-list"),
                tool_name: BuiltinToolName::CodeSymbols.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "action": "file_symbols", "path": "src/auth.rs" })
                    .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(list.status, ExecutionResultStatus::Succeeded);
        let list_payload: Value = serde_json::from_str(&list.payload).expect("list json");
        let names: Vec<&str> = list_payload["results"]
            .as_array()
            .expect("list results")
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert!(
            names.contains(&"authenticate_user"),
            "file_symbols 应含函数，实际: {names:?}"
        );
        assert!(
            names.contains(&"Session"),
            "file_symbols 应含 struct，实际: {names:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
