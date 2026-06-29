use magi_core::{AccessProfile, ApprovalRequirement, ExecutionResultStatus, RiskLevel};
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestrictedWriteProfilePolicy {
    AutoAllowed,
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
            "file_read" => Some(Self::FileRead),
            "view_image" => Some(Self::ViewImage),
            "file_write" => Some(Self::FileWrite),
            "file_patch" => Some(Self::FilePatch),
            "apply_patch" => Some(Self::ApplyPatch),
            "file_remove" => Some(Self::FileRemove),
            "file_mkdir" => Some(Self::FileMkdir),
            "file_copy" => Some(Self::FileCopy),
            "file_move" => Some(Self::FileMove),
            "search_text" => Some(Self::SearchText),
            "search_semantic" => Some(Self::SearchSemantic),
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
            "knowledge_query" => Some(Self::KnowledgeQuery),
            "code_symbols" => Some(Self::CodeSymbols),
            "tool_catalog" => Some(Self::ToolCatalog),
            "agent_spawn" => Some(Self::AgentSpawn),
            "agent_wait" => Some(Self::AgentWait),
            "todo_write" => Some(Self::TodoWrite),
            "memory_write" => Some(Self::MemoryWrite),
            "mission_charter_write" => Some(Self::MissionCharterWrite),
            "plan_write" => Some(Self::PlanWrite),
            "kg_write" => Some(Self::KgWrite),
            "validation_record" => Some(Self::ValidationRecord),
            "checkpoint_create" => Some(Self::Checkpoint),
            "human_checkpoint_request" => Some(Self::HumanCheckpointRequest),
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

    /// 受限访问模式下写工具在 AccessProfile 轴的策略。
    ///
    /// AutoAllowed 不是最终执行许可：输入敏感工具仍会继续经过
    /// invocation policy / governance / SafetyGate 判定。
    pub(crate) fn restricted_write_profile_policy(&self) -> Option<RestrictedWriteProfilePolicy> {
        let policy = match self {
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
            | Self::HumanCheckpointRequest => RestrictedWriteProfilePolicy::AutoAllowed,
            _ => return None,
        };
        Some(policy)
    }

    pub(crate) fn captures_workspace_changes(&self) -> bool {
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
                    "max_bytes": { "type": "integer", "description": "action=read 时 stdout / stderr 预览最多读取的字节数" },
                    "access_mode": {
                        "type": "string",
                        "description": "声明命令访问模式：read_only / maybe_write / explicit_write。ls、cat、grep、git status、git diff、不会改文件的测试等只读探查请用 read_only。read_only 必须实际不写文件：不得创建、修改、删除文件，不得把输出重定向到普通文件或临时文件（如 > /tmp/...、>> file），不得使用 tee、touch、mktemp、rm、mv、cp 等写类操作；仅允许在条件探测中把输出丢弃到 /dev/null。需要 scratch 文件、缓存结果或任何写入时必须声明 maybe_write 或 explicit_write，或改用管道/标准输出完成验证。只读探测中“文件不存在/无匹配”属于可汇报结果时，命令必须用 if/else、|| true 或末尾 true 保证整体退出码为 0，避免把可恢复探测误判为任务失败。",
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
                    "input": { "type": "string", "description": "要写入进程 stdin 的文本" }
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
                    "after_label": { "type": "string", "description": "更新侧的标签" }
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
                        "enum": ["definition", "goto_definition", "file_symbols", "list_file_symbols"],
                        "description": "definition/goto_definition：按符号名查定义；file_symbols/list_file_symbols：列出某文件的全部符号"
                    },
                    "name": { "type": "string", "description": "action=definition/goto_definition 时的符号名（函数/类/接口/类型等）" },
                    "path": { "type": "string", "description": "action=file_symbols/list_file_symbols 时的文件路径（相对工作区根）" },
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
                                    "description": "步骤状态，必须显式提供。"
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
                            "required": ["id", "content", "status"]
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
            BuiltinToolAccessMode::ExplicitWrite
                if tool.restricted_write_profile_policy()
                    == Some(RestrictedWriteProfilePolicy::AutoAllowed) =>
            {
                engine.register_restricted_auto_write_tool(tool.as_str());
            }
            BuiltinToolAccessMode::MaybeWrite => {}
            BuiltinToolAccessMode::ExplicitWrite => {}
        }
    }
    engine
}

pub(crate) const TOOL_POLICY_REJECTED_PUBLIC_ERROR: &str = "该工具在当前上下文中不可用";
pub(crate) const TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR: &str =
    "受限访问已拦截该操作，请切换为完全访问权限后重试";

pub(crate) fn tool_policy_decision_payload(
    tool_name: &str,
    status: ExecutionResultStatus,
    reason: &str,
    access_profile: AccessProfile,
) -> String {
    let (status_label, error_code, public_error) = match status {
        ExecutionResultStatus::NeedsApproval => (
            "needs_approval",
            "tool_policy_needs_approval",
            TOOL_POLICY_NEEDS_APPROVAL_PUBLIC_ERROR,
        ),
        ExecutionResultStatus::Rejected => (
            "rejected",
            "tool_policy_rejected",
            TOOL_POLICY_REJECTED_PUBLIC_ERROR,
        ),
        _ => ("failed", "tool_policy_failed", "该工具暂不可用"),
    };
    tracing::warn!(
        tool_name,
        status = status_label,
        access_profile = access_profile.as_str(),
        reason,
        "tool registry policy decision"
    );
    serde_json::json!({
        "tool": tool_name,
        "status": status_label,
        "error_code": error_code,
        "error": public_error,
        "access_profile": access_profile.as_str(),
    })
    .to_string()
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

    pub(crate) fn is_writeful(&self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "read" | "read_only" | "readonly" => Some(Self::ReadOnly),
            "maybe" | "maybe_write" | "maybewrite" => Some(Self::MaybeWrite),
            "write" | "explicit_write" | "explicitwrite" => Some(Self::ExplicitWrite),
            _ => None,
        }
    }
}

pub(crate) fn low_risk_policy() -> BuiltinToolInvocationPolicy {
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

    let action =
        json_field_string(&request, &["action"]).map(|value| value.trim().to_ascii_lowercase());
    let has_terminal_id = request.get("terminal_id").is_some();
    let has_command =
        json_field_string(&request, &["command"]).is_some_and(|value| !value.trim().is_empty());

    match action.as_deref() {
        None if has_terminal_id && !has_command => return low_risk_policy(),
        Some("read" | "list") => return low_risk_policy(),
        Some("write" | "kill") => {
            return medium_risk_policy();
        }
        Some("run") | None => {}
        Some(_) => return medium_risk_policy(),
    }

    if json_field_bool(&request, &["background"]).unwrap_or(false) {
        return medium_risk_policy();
    }

    match json_field_string(&request, &["access_mode"])
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
    if json_field_string(&request, &["path"]).is_none_or(|path| path.trim().is_empty()) {
        return medium_risk_policy();
    }
    high_risk_approval_policy()
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
