//! Prompt IR、缓存身份和断代模型的唯一运行时事实。
//!
//! Provider 缓存通常按渲染后的前缀匹配，而不是按“这是同一个会话”匹配。
//! 因此这里同时保存稳定段、追加历史、工具面和易变尾部的规范化摘要；调用方
//! 不能自行拼接 `prompt_cache_key`，必须通过这里生成稳定的传输投影。

use magi_bridge_client::ChatMessageOrigin;
#[cfg(test)]
use magi_bridge_client::ModelInvocationRequest;
use magi_core::SessionId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LINEAGE_VERSION: &str = "magi-prompt-lineage-v2";
const CACHE_KEY_VERSION: &str = "magi-prompt-cache-v2";
const RENDER_VERSION: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptBranchScope {
    Orchestrator,
    Worker { task_id: String },
    Auxiliary { purpose: String },
}

impl PromptBranchScope {
    fn canonical(&self) -> String {
        match self {
            Self::Orchestrator => "orchestrator".to_string(),
            Self::Worker { task_id } => format!("worker:{task_id}"),
            Self::Auxiliary { purpose } => format!("auxiliary:{purpose}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCacheScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_profile: Option<String>,
}

impl ProviderCacheScope {
    pub fn from_client(client: &dyn magi_bridge_client::ModelBridgeClient) -> Option<Self> {
        let identity = client.cache_identity()?;
        Some(Self {
            provider: Some(
                match identity.protocol.as_str() {
                    "anthropic_messages" => "anthropic",
                    "openai_chat" | "openai_responses" => "openai",
                    _ => "unknown",
                }
                .into(),
            ),
            protocol: Some(identity.protocol),
            endpoint_fingerprint: Some(digest(&[&identity.endpoint])),
            resolved_model: Some(identity.model),
            reasoning_profile: Some(digest(&[&identity.reasoning_profile])),
        })
    }
}

pub use magi_session_store::CacheInvalidationReason;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptLineage {
    pub lineage_id: String,
    pub cache_epoch: u64,
    pub branch_scope: PromptBranchScope,
    #[serde(default)]
    pub provider_scope: ProviderCacheScope,
    pub render_version: u32,
    pub invalidation_reason: Option<CacheInvalidationReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSegmentKind {
    StaticPolicy,
    SessionStable,
    AppendOnlyHistory,
    ToolSurface,
    RoundContext,
    ProviderContext,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSegment {
    pub kind: PromptSegmentKind,
    pub ordinal: usize,
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptFrame {
    /// 规范化规则版本。规则改变时必须产生新的 epoch，不能静默复用旧缓存。
    pub ir_version: u32,
    /// 稳定规则与会话上下文的有序摘要，不包括可追加历史及易变尾部。
    pub stable_prefix_hash: String,
    /// 对话历史的有序摘要；追加一条历史只会改变这一段及其后缀。
    #[serde(default)]
    pub append_only_history_hash: String,
    pub tool_surface_hash: String,
    pub volatile_tail_hash: String,
    /// 稳定前缀中的私有上下文。历史中的上下文归哈希链，尾部中的上下文归
    /// volatile_tail_hash；正常追加 thinking/signature 不得轮换缓存代际。
    #[serde(default)]
    pub provider_context_hash: String,
    /// 易变段之后意外出现的内容。正常装配应为空；保留它可以在诊断中发现
    /// “稳定内容被放到了缓存断点之后”的结构性错误。
    #[serde(default)]
    pub post_boundary_hash: String,
    pub skill_hash: String,
    pub history_chain_hash: String,
    pub history_message_count: usize,
    /// 仅在本次装配时用于检验前驱链，不持久化每条历史的摘要。
    #[serde(skip)]
    history_prefix_hashes: Vec<String>,
    /// 请求视图对已有历史的改写量，由装配请求的调用方声明；缺省表示未声明。
    /// 它是归因元数据，不属于请求内容身份，不参与请求指纹。
    #[serde(skip)]
    pub history_projection: Option<magi_session_store::HistoryProjection>,
    /// 易变尾部之前（含工具表）与尾部的序列化字节数，见 [`cache_reuse_account`]。
    pub prefix_bytes: u64,
    pub tail_bytes: u64,
    pub segments: Vec<PromptSegment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptFrameDiff {
    pub stable_prefix_changed: bool,
    pub history_changed: bool,
    pub tool_surface_changed: bool,
    pub provider_context_changed: bool,
    pub volatile_tail_changed: bool,
    pub first_different_segment: Option<PromptSegmentKind>,
}

impl PromptFrame {
    /// 纯编译器测试适配入口。生产请求必须从实际 Bridge 取得 IR，不能绕过
    /// 工具 wire name 编码或连接能力处理，自行摘要原始请求。
    #[cfg(test)]
    pub(crate) fn from_request(request: &ModelInvocationRequest) -> Self {
        let ir = magi_bridge_client::prompt_ir::compile_prompt_ir(request, false, None, None);
        Self::from_ir(&ir)
    }

    /// 摘要读取 renderer 消费的同一份规范化正文 IR，不再对原始请求另做一套投影。
    pub fn from_ir(ir: &magi_bridge_client::prompt_ir::PromptIr) -> Self {
        let mut stable_prefix_parts = Vec::new();
        let mut history_parts = Vec::new();
        let mut volatile_parts = Vec::new();
        let mut provider_context_parts = Vec::new();
        let mut post_boundary_parts = Vec::new();
        let mut skill_parts = Vec::new();
        let mut history_prefix_hashes = vec![digest(&["magi-history-chain-v1"])];
        let mut segments = Vec::new();
        let (mut prefix_bytes, mut tail_bytes) = (0u64, 0u64);
        let history_end = ir.history_end();
        for (index, message) in ir.messages.iter().enumerate() {
            let in_volatile_tail = index >= history_end;
            let kind = match message.origin {
                ChatMessageOrigin::StaticPolicy => PromptSegmentKind::StaticPolicy,
                ChatMessageOrigin::SystemInstruction
                | ChatMessageOrigin::ProjectContext
                | ChatMessageOrigin::Skill
                | ChatMessageOrigin::ContextReference => PromptSegmentKind::SessionStable,
                ChatMessageOrigin::Conversation => PromptSegmentKind::AppendOnlyHistory,
                ChatMessageOrigin::RoundContext => PromptSegmentKind::RoundContext,
            };
            // Provider 私有 thinking/signature/output item 独立成段，避免一个
            // provider context 的变化把普通历史段误标成规则或工具面变化。
            let mut message_without_provider_context = message.clone();
            let mut provider_context = Vec::new();
            if let magi_bridge_client::llm_types::LlmMessageContent::Blocks(blocks) =
                &mut message_without_provider_context.content
            {
                blocks.retain(|block| {
                    if let magi_bridge_client::llm_types::LlmContentBlock::ProviderContext {
                        context,
                    } = block
                    {
                        provider_context.push(context.clone());
                        false
                    } else {
                        true
                    }
                });
                let text = match blocks.as_slice() {
                    [] => Some(String::new()),
                    [magi_bridge_client::llm_types::LlmContentBlock::Text { text }] => {
                        Some(text.clone())
                    }
                    _ => None,
                };
                if let Some(text) = text {
                    message_without_provider_context.content =
                        magi_bridge_client::llm_types::LlmMessageContent::Text(text);
                }
            }
            let value = serde_json::to_value(&message_without_provider_context)
                .unwrap_or(serde_json::Value::Null);
            let hash = digest_json(&value);
            let message_bytes = (value.to_string().len()
                + if provider_context.is_empty() {
                    0
                } else {
                    serde_json::to_string(&provider_context).map_or(0, |text| text.len())
                }) as u64;
            if in_volatile_tail {
                tail_bytes += message_bytes;
            } else {
                prefix_bytes += message_bytes;
            }
            if !in_volatile_tail && message.origin == ChatMessageOrigin::Skill {
                skill_parts.push(hash.clone());
            }
            let provider_hash = digest_json(
                &serde_json::to_value(&provider_context).expect("provider context serializes"),
            );
            if !in_volatile_tail && kind == PromptSegmentKind::AppendOnlyHistory {
                // 私有上下文也属于真实 wire 历史；旧 signature 被改写同样会破坏前缀。
                history_prefix_hashes.push(digest(&[
                    history_prefix_hashes.last().expect("chain seed"),
                    &hash,
                    &provider_hash,
                ]));
            }
            let segment = PromptSegment {
                kind,
                ordinal: segments.len(),
                content_hash: hash.clone(),
            };
            if in_volatile_tail {
                // 与 renderer 共用首个 RoundContext 边界；其后的恢复说明即使
                // 仍带 Conversation 来源，也不能进入历史链或稳定前缀。
                let tail_hash = digest_json(&serde_json::json!({
                    "kind": kind, "hash": hash, "providerContext": provider_hash,
                }));
                volatile_parts.push(tail_hash.clone());
                if kind != PromptSegmentKind::RoundContext {
                    post_boundary_parts.push(tail_hash);
                }
            } else if kind == PromptSegmentKind::AppendOnlyHistory {
                history_parts.push(hash.clone());
            } else {
                stable_prefix_parts.push(digest_json(&serde_json::json!({
                    "kind": kind, "hash": hash,
                })));
                if !provider_context.is_empty() {
                    provider_context_parts.push(digest_json(&serde_json::json!({
                        "message": index, "kind": kind, "hash": provider_hash,
                    })));
                }
            }
            segments.push(segment);
            if !provider_context.is_empty() {
                let provider_segment = PromptSegment {
                    kind: PromptSegmentKind::ProviderContext,
                    ordinal: segments.len(),
                    content_hash: provider_hash.clone(),
                };
                segments.push(provider_segment);
            }
        }
        let tool_surface_hash = digest_json(&serde_json::json!(ir.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "definition": tool,
                        "origin": tool.origin,
                    })
                })
                .collect::<Vec<_>>()
        })));
        prefix_bytes += ir.tools.as_ref().map_or(0, |tools| {
            serde_json::to_string(tools).map_or(0, |text| text.len())
        }) as u64;
        segments.push(PromptSegment {
            kind: PromptSegmentKind::ToolSurface,
            ordinal: segments.len(),
            content_hash: tool_surface_hash.clone(),
        });
        Self {
            ir_version: RENDER_VERSION,
            stable_prefix_hash: digest_strings(&stable_prefix_parts),
            append_only_history_hash: digest_strings(&history_parts),
            tool_surface_hash,
            volatile_tail_hash: digest_strings(&volatile_parts),
            provider_context_hash: digest_strings(&provider_context_parts),
            post_boundary_hash: digest_strings(&post_boundary_parts),
            skill_hash: digest_strings(&skill_parts),
            history_chain_hash: history_prefix_hashes.last().expect("chain seed").clone(),
            history_message_count: history_prefix_hashes.len() - 1,
            history_prefix_hashes,
            history_projection: None,
            prefix_bytes,
            tail_bytes,
            segments,
        }
    }

    /// 所有调用共用的断代规则：实际连接、规则、工具面、压缩代际和执行权限。
    /// 追加历史、易变尾部、设置 revision 和输出上限均不参与。
    pub fn cache_epoch(
        &self,
        provider_scope: &ProviderCacheScope,
        history_generation: u64,
        execution_scope: Option<&magi_bridge_client::ModelExecutionScope>,
    ) -> u64 {
        let identity = digest_json(&serde_json::json!({
            "providerScope": provider_scope,
            "staticPrefix": self.stable_prefix_hash,
            "toolSurface": self.tool_surface_hash,
            "skillPrefix": self.skill_hash,
            // 只有稳定前缀的私有上下文参与基础代际。历史的追加/改写由
            // history_prefix_hashes 裁决，易变尾部不能参与缓存代际。
            "providerContext": self.provider_context_hash,
            "renderer": self.ir_version,
            "historyGeneration": history_generation,
            "executionScope": execution_scope.map(|scope| serde_json::json!({
                "workingDir": scope.working_dir,
                "policy": scope.policy,
                "requiresNativeTools": scope.requires_native_tools,
                "closedSkillScope": scope.closed_skill_scope,
            })),
        }));
        u64::from_str_radix(&identity[..16], 16).expect("SHA-256 prefix is hexadecimal")
    }

    pub fn digest(&self) -> String {
        digest(&[
            &self.ir_version.to_string(),
            &self.stable_prefix_hash,
            &self.append_only_history_hash,
            &self.tool_surface_hash,
            &self.volatile_tail_hash,
            &self.provider_context_hash,
            &self.post_boundary_hash,
            &digest_json(&serde_json::to_value(&self.segments).expect("segments serialize")),
        ])
    }

    /// 绑定真实连接与 canonical 前驱。所有请求共用这一失效策略；前驱只来自
    /// 已提交 Step，不从 settings revision、UI 投影或本地临时缓存推断。
    pub fn bind_lineage(
        &self,
        mut lineage: PromptLineage,
        history_generation: u64,
        execution_scope: Option<&magi_bridge_client::ModelExecutionScope>,
        previous: Option<&magi_session_store::ModelStep>,
    ) -> (PromptLineage, magi_session_store::ModelCacheBinding) {
        let scope = &lineage.provider_scope;
        let mut binding = magi_session_store::ModelCacheBinding {
            lineage_id: lineage.lineage_id.clone(),
            cache_key: String::new(),
            provider_hash: digest_json(&serde_json::json!({
                "provider": scope.provider,
                "protocol": scope.protocol,
                "endpoint": scope.endpoint_fingerprint,
            })),
            model_hash: digest_json(&serde_json::json!(scope.resolved_model)),
            reasoning_hash: digest_json(&serde_json::json!(scope.reasoning_profile)),
            execution_scope_hash: digest_json(&serde_json::json!(execution_scope.map(|scope| {
                serde_json::json!({
                    "workingDir": scope.working_dir,
                    "policy": scope.policy,
                    "requiresNativeTools": scope.requires_native_tools,
                })
            }))),
            skill_scope_hash: digest_json(&serde_json::json!({
                "prompt": self.skill_hash,
                "closedScope": execution_scope.map(|scope| &scope.closed_skill_scope),
            })),
            history_epoch_hash: None,
            invalidation_reason: Some(CacheInvalidationReason::Initial),
        };
        if let Some((step, frame, old)) = previous.and_then(|step| {
            let frame = step.prompt_frame.as_ref()?;
            let old = frame.cache_binding.as_ref()?;
            (old.lineage_id == lineage.lineage_id).then_some((step, frame, old))
        }) {
            let reason = if self.ir_version != frame.ir_version {
                Some(CacheInvalidationReason::RendererChanged)
            } else if binding.provider_hash != old.provider_hash {
                Some(CacheInvalidationReason::ProviderChanged)
            } else if binding.model_hash != old.model_hash {
                Some(CacheInvalidationReason::ModelChanged)
            } else if binding.reasoning_hash != old.reasoning_hash {
                Some(CacheInvalidationReason::ReasoningChanged)
            } else if binding.execution_scope_hash != old.execution_scope_hash {
                Some(CacheInvalidationReason::PermissionChanged)
            } else if binding.skill_scope_hash != old.skill_scope_hash {
                Some(CacheInvalidationReason::SkillChanged)
            } else if self.tool_surface_hash != frame.tool_surface_hash {
                Some(CacheInvalidationReason::ToolSurfaceChanged)
            } else if self.provider_context_hash != frame.provider_context_hash {
                Some(CacheInvalidationReason::ProviderContextChanged)
            } else if self.stable_prefix_hash != frame.stable_prefix_hash {
                Some(CacheInvalidationReason::StablePrefixChanged)
            } else if history_generation != step.checkpoint_generation {
                Some(CacheInvalidationReason::Compaction)
            } else {
                None
            };
            binding.invalidation_reason = reason;
            if reason.is_none() {
                binding.history_epoch_hash = old.history_epoch_hash.clone();
                // 独立摘要/标题等辅助请求没有追加历史合同，内容更新不是历史改写。
                if !matches!(lineage.branch_scope, PromptBranchScope::Auxiliary { .. })
                    && let Some((count, hash)) = frame
                        .history_message_count
                        .zip(frame.history_chain_hash.as_ref())
                    && self.history_prefix_hashes.get(count) != Some(hash)
                {
                    // 截回上一次改写后的边界也是一次新的断代，不能因正文摘要
                    // 恰好回到旧值就复用旧 epoch。链入前驱代际，追加时继续沿用。
                    binding.history_epoch_hash = Some(digest(&[
                        old.history_epoch_hash
                            .as_deref()
                            .unwrap_or("magi-history-epoch-v1"),
                        &self.history_chain_hash,
                    ]));
                    binding.invalidation_reason = Some(self.declared_rewrite_reason(frame));
                }
            }
        }
        let base_epoch = self.cache_epoch(scope, history_generation, execution_scope);
        let epoch = binding
            .history_epoch_hash
            .as_deref()
            .map_or(base_epoch, |history| {
                let hash = digest(&[&base_epoch.to_string(), history]);
                u64::from_str_radix(&hash[..16], 16).expect("SHA-256 prefix is hexadecimal")
            });
        lineage.cache_epoch = epoch;
        lineage.invalidation_reason = binding.invalidation_reason;
        binding.cache_key = lineage.cache_key();
        (lineage, binding)
    }

    /// 已发送历史被改写时，按投影声明归因。计数增长说明改写来自总预算降级或文件事实失效；
    /// 没有任何声明的改写属于未预期的前缀破坏，保持 `HistoryRewritten` 并记录警告。
    fn declared_rewrite_reason(
        &self,
        previous: &magi_session_store::ModelPromptFrame,
    ) -> CacheInvalidationReason {
        let before = previous.history_projection.unwrap_or_default();
        let now = self.history_projection.unwrap_or_default();
        if now.stale_file_facts > before.stale_file_facts {
            CacheInvalidationReason::FileFactsRefreshed
        } else if now.demoted_tool_results > before.demoted_tool_results {
            CacheInvalidationReason::ToolHistoryDemoted
        } else {
            tracing::warn!(
                demoted = now.demoted_tool_results,
                stale = now.stale_file_facts,
                "已发送历史被改写，但没有任何投影声明；前缀可能被意外破坏"
            );
            CacheInvalidationReason::HistoryRewritten
        }
    }

    pub fn diff(&self, previous: &Self) -> PromptFrameDiff {
        let stable_prefix_changed = self.stable_prefix_hash != previous.stable_prefix_hash;
        let history_changed = self.append_only_history_hash != previous.append_only_history_hash;
        let tool_surface_changed = self.tool_surface_hash != previous.tool_surface_hash;
        let provider_context_changed = self
            .segments
            .iter()
            .filter(|segment| segment.kind == PromptSegmentKind::ProviderContext)
            .ne(previous
                .segments
                .iter()
                .filter(|segment| segment.kind == PromptSegmentKind::ProviderContext));
        let volatile_tail_changed = self.volatile_tail_hash != previous.volatile_tail_hash;
        let first_different_segment = self
            .segments
            .iter()
            .zip(&previous.segments)
            .find(|(current, old)| {
                current.kind != old.kind || current.content_hash != old.content_hash
            })
            .map(|(current, _)| current.kind)
            .or_else(|| {
                self.segments
                    .get(previous.segments.len())
                    .map(|segment| segment.kind)
            })
            .or_else(|| {
                previous
                    .segments
                    .get(self.segments.len())
                    .map(|segment| segment.kind)
            });
        PromptFrameDiff {
            stable_prefix_changed,
            history_changed,
            tool_surface_changed,
            provider_context_changed,
            volatile_tail_changed,
            first_different_segment,
        }
    }
}

impl PromptLineage {
    pub fn for_session(session_id: &SessionId) -> Self {
        Self::for_branch(session_id, PromptBranchScope::Orchestrator)
    }

    pub fn for_branch(session_id: &SessionId, branch_scope: PromptBranchScope) -> Self {
        let branch = branch_scope.canonical();
        Self {
            lineage_id: digest(&[LINEAGE_VERSION, session_id.as_str(), &branch]),
            cache_epoch: 0,
            branch_scope,
            provider_scope: ProviderCacheScope::default(),
            render_version: RENDER_VERSION,
            invalidation_reason: Some(CacheInvalidationReason::Initial),
        }
    }

    pub fn with_provider_scope(mut self, provider_scope: ProviderCacheScope) -> Self {
        self.provider_scope = provider_scope;
        self
    }

    pub fn with_epoch(mut self, cache_epoch: u64, reason: CacheInvalidationReason) -> Self {
        self.cache_epoch = cache_epoch;
        self.invalidation_reason = Some(reason);
        self
    }

    /// 仅暴露固定长度摘要，不暴露 session、task、provider 或模型原文。
    pub fn cache_key(&self) -> String {
        let provider = digest_json(
            &serde_json::to_value(&self.provider_scope).expect("cache scope serializes"),
        );
        let branch = self.branch_scope.canonical();
        let epoch = self.cache_epoch.to_string();
        let render = self.render_version.to_string();
        digest(&[
            CACHE_KEY_VERSION,
            &self.lineage_id,
            &provider,
            &branch,
            &epoch,
            &render,
        ])
    }
}

/// 一次请求的缓存核算：把输入拆成 Magi 能控制的部分和 provider 应当服务的部分。
/// 字节比例只是 Token 比例的近似；用于判断前缀是否被 Magi 破坏，不用于计费。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheReuseAccount {
    /// 上一请求的前缀在本请求里仍是前缀，按理应当被服务的 Token 数。
    pub expected_cached_tokens: u64,
    /// 每次请求都要重发的易变尾部 Token 数。
    pub volatile_tail_tokens: u64,
    /// 应当命中却没有命中的部分：前缀由 Magi 保持稳定，因此归因到 provider 侧（冷启动、
    /// 复用粒度、淘汰、路由）。
    pub provider_shortfall_tokens: u64,
}

/// 核算本请求相对同一 lineage 上一请求的缓存预期。新 epoch（任何断代）或缺少尺寸记录时
/// 没有预期，返回 `None`；断代本身已经由 `invalidation_reason` 声明。
pub fn cache_reuse_account(
    previous: Option<&magi_session_store::ModelPromptFrame>,
    current: &magi_session_store::ModelPromptFrame,
    input_tokens: u64,
    cache_read_tokens: u64,
) -> Option<CacheReuseAccount> {
    let binding = current.cache_binding.as_ref()?;
    if binding.invalidation_reason.is_some() {
        return None;
    }
    let (previous_prefix, prefix, tail) = (
        previous?.prefix_bytes?,
        current.prefix_bytes?,
        current.tail_bytes?,
    );
    let total = prefix.checked_add(tail).filter(|total| *total > 0)?;
    let share =
        |bytes: u64| ((input_tokens as u128 * bytes.min(total) as u128) / total as u128) as u64;
    let expected_cached_tokens = share(previous_prefix);
    Some(CacheReuseAccount {
        expected_cached_tokens,
        volatile_tail_tokens: share(tail),
        provider_shortfall_tokens: expected_cached_tokens.saturating_sub(cache_read_tokens),
    })
}

fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn digest_strings(parts: &[String]) -> String {
    digest(&parts.iter().map(String::as_str).collect::<Vec<_>>())
}

fn digest_json(value: &serde_json::Value) -> String {
    let canonical = magi_bridge_client::llm_types::canonicalize_json(value);
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    digest(&[&String::from_utf8_lossy(&bytes)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized_frame(
        prefix: u64,
        tail: u64,
        reason: Option<CacheInvalidationReason>,
    ) -> magi_session_store::ModelPromptFrame {
        let hash = "a".repeat(64);
        magi_session_store::ModelPromptFrame {
            ir_version: RENDER_VERSION,
            stable_prefix_hash: hash.clone(),
            append_only_history_hash: hash.clone(),
            tool_surface_hash: hash.clone(),
            volatile_tail_hash: hash.clone(),
            provider_context_hash: hash.clone(),
            post_boundary_hash: hash.clone(),
            history_message_count: Some(1),
            history_chain_hash: Some(hash.clone()),
            history_projection: None,
            prefix_bytes: Some(prefix),
            tail_bytes: Some(tail),
            cache_binding: Some(magi_session_store::ModelCacheBinding {
                lineage_id: hash.clone(),
                cache_key: hash.clone(),
                provider_hash: hash.clone(),
                model_hash: hash.clone(),
                reasoning_hash: hash.clone(),
                execution_scope_hash: hash.clone(),
                skill_scope_hash: hash,
                history_epoch_hash: None,
                invalidation_reason: reason,
            }),
        }
    }

    #[test]
    fn reuse_account_splits_stable_prefix_tail_and_provider_shortfall() {
        let previous = sized_frame(8_000, 1_000, None);
        let current = sized_frame(8_500, 1_000, None);
        // 总字节 9500：上一请求的前缀占 8000/9500，尾部占 1000/9500。
        let account = cache_reuse_account(Some(&previous), &current, 9_500, 7_000).unwrap();
        assert_eq!(account.expected_cached_tokens, 8_000);
        assert_eq!(account.volatile_tail_tokens, 1_000);
        assert_eq!(account.provider_shortfall_tokens, 1_000);
        // provider 服务了全部可复用前缀：没有缺口。
        let served = cache_reuse_account(Some(&previous), &current, 9_500, 8_064).unwrap();
        assert_eq!(served.provider_shortfall_tokens, 0);
    }

    #[test]
    fn reuse_account_has_no_expectation_after_a_declared_epoch_change_or_without_sizes() {
        let previous = sized_frame(8_000, 1_000, None);
        let rotated = sized_frame(8_500, 1_000, Some(CacheInvalidationReason::ModelChanged));
        assert!(cache_reuse_account(Some(&previous), &rotated, 9_500, 0).is_none());
        assert!(cache_reuse_account(None, &sized_frame(1, 1, None), 10, 0).is_none());
        let mut unsized_frame = sized_frame(8_500, 1_000, None);
        unsized_frame.prefix_bytes = None;
        assert!(cache_reuse_account(Some(&previous), &unsized_frame, 9_500, 0).is_none());
    }

    #[test]
    fn lineage_is_deterministic_and_does_not_leak_identity() {
        let session = SessionId::new("session-secret-value");
        let left = PromptLineage::for_session(&session);
        let right = PromptLineage::for_session(&session);
        assert_eq!(left, right);
        assert_eq!(left.lineage_id.len(), 64);
        assert_eq!(left.cache_key().len(), 64);
        assert!(!left.cache_key().contains("session-secret-value"));
    }

    #[test]
    fn branch_provider_and_epoch_are_separate_cache_namespaces() {
        let session = SessionId::new("session");
        let main = PromptLineage::for_session(&session);
        let worker = PromptLineage::for_branch(
            &session,
            PromptBranchScope::Worker {
                task_id: "task-1".into(),
            },
        );
        let auxiliary = PromptLineage::for_branch(
            &session,
            PromptBranchScope::Auxiliary {
                purpose: "title".into(),
            },
        );
        assert_ne!(main.cache_key(), worker.cache_key());
        assert_ne!(main.cache_key(), auxiliary.cache_key());
        assert_ne!(
            main.cache_key(),
            main.clone()
                .with_epoch(1, CacheInvalidationReason::Compaction)
                .cache_key()
        );
        assert_ne!(
            main.cache_key(),
            main.with_provider_scope(ProviderCacheScope {
                resolved_model: Some("other-model".into()),
                ..ProviderCacheScope::default()
            })
            .cache_key()
        );
    }

    #[test]
    fn prompt_frame_keeps_round_context_out_of_stable_prefix() {
        let message = |origin: ChatMessageOrigin, content: &str| magi_bridge_client::ChatMessage {
            input_request_id: None,
            context_origin: origin,
            role: "user".into(),
            content: Some(content.into()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        };
        let request = |tail| ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: "prompt".into(),
            messages: Some(vec![
                message(ChatMessageOrigin::Conversation, "history"),
                message(ChatMessageOrigin::RoundContext, tail),
            ]),
            tools: None,
            tool_choice: None,
        };
        let first = PromptFrame::from_request(&request("tail-a"));
        let second = PromptFrame::from_request(&request("tail-b"));
        assert_eq!(first.stable_prefix_hash, second.stable_prefix_hash);
        assert_ne!(first.volatile_tail_hash, second.volatile_tail_hash);
        assert!(
            first
                .segments
                .iter()
                .any(|segment| { segment.kind == PromptSegmentKind::RoundContext })
        );
    }

    #[test]
    fn append_only_history_does_not_rotate_stable_cache_epoch() {
        let message = |content: &str| magi_bridge_client::ChatMessage {
            input_request_id: None,
            context_origin: ChatMessageOrigin::Conversation,
            role: "user".into(),
            content: Some(content.into()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        };
        let request = |messages| ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(messages),
            tools: None,
            tool_choice: None,
        };
        let first = PromptFrame::from_request(&request(vec![message("one")]));
        let second = PromptFrame::from_request(&request(vec![message("one"), message("two")]));
        assert_eq!(first.stable_prefix_hash, second.stable_prefix_hash);
        assert_ne!(
            first.append_only_history_hash,
            second.append_only_history_hash
        );
        let scope = ProviderCacheScope::default();
        assert_eq!(
            first.cache_epoch(&scope, 0, None),
            second.cache_epoch(&scope, 0, None)
        );
    }

    #[test]
    fn provider_context_isolated_from_the_regular_history_segment() {
        let mut message = magi_bridge_client::ChatMessage {
            input_request_id: None,
            context_origin: ChatMessageOrigin::Conversation,
            role: "assistant".into(),
            content: Some("answer".into()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: vec![magi_bridge_client::ModelProviderContext {
                provider: "test".into(),
                kind: "reasoning_signature".into(),
                data: serde_json::json!("opaque"),
            }],
        };
        let first = PromptFrame::from_request(&ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(vec![message.clone()]),
            tools: None,
            tool_choice: None,
        });
        message.provider_context[0].data = serde_json::json!("changed");
        let second = PromptFrame::from_request(&ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(vec![message]),
            tools: None,
            tool_choice: None,
        });
        assert_eq!(
            first.append_only_history_hash,
            second.append_only_history_hash
        );
        assert_ne!(first.digest(), second.digest());
        // 历史私有上下文归历史链；基础 epoch 不能随着正常追加 reasoning 变化。
        // 绑定前驱时再区分追加与改写，避免每条 thinking 都轮换缓存键。
        let scope = ProviderCacheScope::default();
        assert_eq!(
            first.cache_epoch(&scope, 0, None),
            second.cache_epoch(&scope, 0, None)
        );
        assert_ne!(first.history_chain_hash, second.history_chain_hash);
        assert!(second.diff(&first).provider_context_changed);
        assert!(
            first
                .segments
                .iter()
                .any(|segment| segment.kind == PromptSegmentKind::ProviderContext)
        );
    }

    #[test]
    fn provider_context_and_messages_after_round_boundary_follow_tail_lifetime() {
        let message = |origin, text: &str| magi_bridge_client::ChatMessage {
            input_request_id: None,
            context_origin: origin,
            role: "assistant".into(),
            content: Some(text.into()),
            images: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            provider_context: vec![magi_bridge_client::ModelProviderContext {
                provider: "openai".into(),
                kind: "response_output_item".into(),
                data: serde_json::json!({"type":"reasoning", "encrypted_content":text}),
            }],
        };
        let mut request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(vec![
                message(ChatMessageOrigin::StaticPolicy, "stable"),
                message(ChatMessageOrigin::Conversation, "history"),
                message(ChatMessageOrigin::RoundContext, "round"),
                message(ChatMessageOrigin::Conversation, "recovery"),
            ]),
            tools: None,
            tool_choice: None,
        };
        let original = PromptFrame::from_request(&request);
        assert_eq!(original.history_message_count, 1);
        let scope = ProviderCacheScope::default();
        for index in [2, 3] {
            let messages = request.messages.as_mut().unwrap();
            messages[index].provider_context[0].data = serde_json::json!({"signature":"new"});
            let changed = PromptFrame::from_request(&request);
            assert_eq!(original.history_chain_hash, changed.history_chain_hash);
            assert_eq!(
                original.provider_context_hash,
                changed.provider_context_hash
            );
            assert_ne!(original.volatile_tail_hash, changed.volatile_tail_hash);
            assert_eq!(
                original.cache_epoch(&scope, 0, None),
                changed.cache_epoch(&scope, 0, None)
            );
        }
        request.messages.as_mut().unwrap()[3].content = Some("new recovery".into());
        let changed = PromptFrame::from_request(&request);
        assert_eq!(original.history_chain_hash, changed.history_chain_hash);
        assert_ne!(original.post_boundary_hash, changed.post_boundary_hash);
        assert_eq!(
            original.cache_epoch(&scope, 0, None),
            changed.cache_epoch(&scope, 0, None)
        );

        request.messages.as_mut().unwrap()[0].provider_context[0].data =
            serde_json::json!({"signature":"new stable context"});
        let changed = PromptFrame::from_request(&request);
        assert_ne!(
            original.provider_context_hash,
            changed.provider_context_hash
        );
        assert_ne!(
            original.cache_epoch(&scope, 0, None),
            changed.cache_epoch(&scope, 0, None)
        );
    }

    #[test]
    fn frame_diff_reports_the_first_changed_segment_without_exposing_content() {
        let make = |tail: &str| ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(vec![magi_bridge_client::ChatMessage {
                input_request_id: None,
                context_origin: ChatMessageOrigin::RoundContext,
                role: "user".into(),
                content: Some(tail.into()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            }]),
            tools: None,
            tool_choice: None,
        };
        let previous = PromptFrame::from_request(&make("old-secret"));
        let current = PromptFrame::from_request(&make("new-secret"));
        let diff = current.diff(&previous);
        assert!(!diff.stable_prefix_changed);
        assert!(diff.volatile_tail_changed);
        assert_eq!(
            diff.first_different_segment,
            Some(PromptSegmentKind::RoundContext)
        );
        let serialized = serde_json::to_string(&diff).unwrap();
        assert!(!serialized.contains("old-secret"));
        assert!(!serialized.contains("new-secret"));
    }

    #[test]
    fn frame_uses_the_same_normalized_body_as_provider_renderers() {
        use magi_bridge_client::{ChatMessage, ChatToolCall, ChatToolFunction};
        let make = |id: &str, arguments: &str| ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(vec![
                ChatMessage {
                    input_request_id: None,
                    context_origin: ChatMessageOrigin::Conversation,
                    role: "assistant".into(),
                    content: None,
                    images: vec![],
                    tool_calls: vec![ChatToolCall {
                        id: id.into(),
                        kind: "function".into(),
                        function: ChatToolFunction {
                            name: "read".into(),
                            arguments: arguments.into(),
                        },
                    }],
                    tool_call_id: None,
                    provider_context: vec![],
                },
                ChatMessage {
                    input_request_id: None,
                    context_origin: ChatMessageOrigin::Conversation,
                    role: "tool".into(),
                    content: Some("result".into()),
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: Some(id.into()),
                    provider_context: vec![],
                },
            ]),
            tools: None,
            tool_choice: None,
        };
        let first = make(" call-1 ", "{\"z\": 1, \"a\": 2}");
        let mut equivalent = make("call-1", "{\"a\":2,\"z\":1}");
        equivalent.tools = Some(vec![]);
        let left = PromptFrame::from_request(&first);
        let ir =
            magi_bridge_client::prompt_ir::compile_prompt_ir(&equivalent, true, None, Some(128));
        assert_eq!(left, PromptFrame::from_ir(&ir));
        assert_eq!(left, PromptFrame::from_request(&equivalent));
        let json = serde_json::to_value(&ir.messages).unwrap();
        assert_eq!(json[0]["content"][0]["id"], "call-1");
        assert_eq!(json[1]["content"][0]["tool_use_id"], "call-1");

        let first = make("", "{\"z\": 1, \"a\": 2}");
        let second = make("", "{\"a\":2,\"z\":1}");
        assert_eq!(
            PromptFrame::from_request(&first),
            PromptFrame::from_request(&second),
            "missing call IDs must be derived from canonical arguments"
        );
    }

    #[test]
    fn static_lifetime_is_part_of_the_prefix_identity() {
        let mut request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(vec![magi_bridge_client::ChatMessage {
                input_request_id: None,
                context_origin: ChatMessageOrigin::StaticPolicy,
                role: "developer".into(),
                content: Some("same text".into()),
                images: vec![],
                tool_calls: vec![],
                tool_call_id: None,
                provider_context: vec![],
            }]),
            tools: None,
            tool_choice: None,
        };
        let first = PromptFrame::from_request(&request);
        request.messages.as_mut().unwrap()[0].context_origin = ChatMessageOrigin::SystemInstruction;
        let second = PromptFrame::from_request(&request);
        assert_ne!(first.stable_prefix_hash, second.stable_prefix_hash);
        assert_ne!(
            first.cache_epoch(&ProviderCacheScope::default(), 0, None),
            second.cache_epoch(&ProviderCacheScope::default(), 0, None)
        );
    }

    #[test]
    fn frame_and_epoch_share_the_transport_canonical_tool_surface() {
        let tool = |name: &str| magi_bridge_client::ChatToolDefinition {
            kind: "function".into(),
            origin: magi_bridge_client::ChatToolOrigin::Builtin,
            function: magi_bridge_client::ChatToolFunctionDefinition {
                name: name.into(),
                description: "inspect".into(),
                parameters: serde_json::json!({"properties":{"b":{"type":"integer"},"a":{"type":"string"}},"type":"object"}),
            },
        };
        let mut request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: "private input".into(),
            messages: None,
            tools: Some(vec![tool("z"), tool("a")]),
            tool_choice: None,
        };
        let original = PromptFrame::from_request(&request);
        request.tools.as_mut().unwrap().reverse();
        let reordered = PromptFrame::from_request(&request);
        let scope = ProviderCacheScope::default();
        assert_eq!(original.tool_surface_hash, reordered.tool_surface_hash);
        assert_eq!(
            original.cache_epoch(&scope, 0, None),
            reordered.cache_epoch(&scope, 0, None)
        );
        request.tools.as_mut().unwrap()[0].function.description = "different".into();
        let changed = PromptFrame::from_request(&request);
        assert_ne!(original.tool_surface_hash, changed.tool_surface_hash);
        assert_ne!(
            original.cache_epoch(&scope, 0, None),
            changed.cache_epoch(&scope, 0, None)
        );
        request.prompt = "different history".into();
        let new_input = PromptFrame::from_request(&request);
        assert_ne!(
            changed.append_only_history_hash,
            new_input.append_only_history_hash
        );
        assert!(
            !serde_json::to_string(&original)
                .unwrap()
                .contains("private input")
        );
    }
}
