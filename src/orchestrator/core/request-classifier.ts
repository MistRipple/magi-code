import type { PlanMode } from '../plan-ledger';
import { buildAllowedToolsOnlyPolicy, type EffectiveToolPolicy } from '../../tools/tool-policy';

export type RequestEntryPath = 'direct_response' | 'lightweight_analysis' | 'task_execution';
export const REQUEST_CLASSIFIER_VERSION = 'heuristic_v4';

export interface RequestEntryPolicy {
  entryPath: RequestEntryPath;
  includeThinking: boolean;
  includeToolCalls: boolean;
  toolPolicy?: EffectiveToolPolicy;
  historyMode: 'session' | 'isolated';
}

const LIGHTWEIGHT_ANALYSIS_ALLOWED_TOOL_NAMES = [
  'file_view',
  'code_search_regex',
  'code_search_semantic',
  'web_search',
  'web_fetch',
  'project_knowledge_query',
] as const;

export interface RequestClassification {
  hasReadOnlyIntent: boolean;
  hasWriteIntent: boolean;
  hasHighImpactIntent: boolean;
  hasWorkspaceScopedIntent: boolean;
  hasAssistantMetaIntent: boolean;
  hasConversationalIntent: boolean;
  isShortConversationalTurn: boolean;
  requiresModification: boolean;
  entryPolicy: RequestEntryPolicy;
  reason: string;
  decisionFactors: string[];
  classifierVersion: string;
}

const READ_ONLY_KEYWORDS = ['分析', '解释', '总结', '查看', '审查', 'review', 'summarize', 'read only'];
const WRITE_KEYWORDS = ['修改', '实现', '修复', '新增', '重构', '删除', '更新', '编写', 'patch'];
const HIGH_IMPACT_KEYWORDS = [
  '架构',
  '迁移',
  '并发',
  'schema',
  'ledger',
  '状态机',
  '依赖',
  '数据库',
  '权限',
  '认证',
  '安全',
  'deploy',
  '生产',
];
const WORKSPACE_SCOPED_KEYWORDS = [
  '当前项目',
  '这个项目',
  '当前代码',
  '代码库',
  '仓库',
  '工作区',
  '模块',
  '文件',
  '目录',
  '技术栈',
  'adr',
  'faq',
  'repo',
  'workspace',
  'codebase',
  'project',
  'module',
  'file',
  'session',
];
const ASSISTANT_META_KEYWORDS = [
  '你是谁',
  '你是什么',
  '你能做什么',
  '你可以做什么',
  '介绍一下你自己',
  '介绍下你自己',
  '自我介绍',
  '你的能力',
  '你的职责',
  '怎么用',
  '如何使用',
  '使用方式',
  '模式区别',
  'magi 是什么',
  '你和 claude',
  '你和 codex',
  '你和 gemini',
  'who are you',
  'what are you',
  'what can you do',
  'how to use',
  'capabilities',
  'your role',
];
const CONVERSATIONAL_KEYWORDS = [
  '你好',
  'hello',
  'hi',
  'thanks',
  'thank you',
  '谢谢',
  '早上好',
  '晚上好',
];
const FILE_LIKE_PATTERN = /(?:^|[\s`'"])(?:\.{0,2}\/|\/)?[\w.-]+\.(?:ts|tsx|js|jsx|vue|py|go|java|json|md|yaml|yml|toml|sh|css|scss|sql|rs)(?=$|[\s`'",:;])/i;
const PATH_LIKE_PATTERN = /(?:^|[\s`'"])(?:[a-z]:\\|\/|\.{1,2}\/)[^\s`'"]+/i;
const CODE_REFERENCE_PATTERN = /(?:\b[A-Za-z_][A-Za-z0-9_]*\.(?:[A-Za-z_][A-Za-z0-9_]*)\s*\(?|\b[A-Za-z_][A-Za-z0-9_]*\(\)|::|=>|#L\d+|\bline\s+\d+\b)/i;
const READ_ONLY_PATTERN = /(?:分析|解释|总结|梳理|审查|review|summarize|analy[sz]e|explain|read[\s-]?only)/i;
const HARD_READ_ONLY_PATTERN = /(?:(?:只做|仅做|只需|仅需|只进行|仅进行)(?:[^。\n；;]{0,18})(?:分析|梳理|评估|任务编排|编排|规划|拆分|review|summarize|analysis)|(?:不(?:要)?修改|不改|勿修改|禁止修改|不触碰)(?:[^。\n；;]{0,12})(?:代码|文件|项目|仓库|源码|页面)?|只读(?:分析|编排)?)/i;
const ORCHESTRATION_PATTERN = /(?:worker_dispatch|worker_wait|任务编排|任务派发|派发任务|任务拆分|assignment|todo_split|编排|派发)/i;
const EXPLICIT_WORKER_DISPATCH_PATTERN = /(?:worker_dispatch|worker_wait|(?:必须|需要|应当|应该|请|先|再|立即|直接|继续|使用|采用|安排|分配|调度|分派|派发|调用).{0,20}(?:worker|多\s*worker|多个\s*worker)|(?:worker|多\s*worker|多个\s*worker).{0,20}(?:分别|协作|执行|处理|审查|分析|分工|编排|调度|分派|派发|调用|wait|dispatch|review|analy[sz]e|implement))/i;
const ASSISTANT_META_PATTERN = /(?:你(?:是|能|可)?做什么|你是谁|你是什么|介绍.*你自己|你的(?:能力|职责)|怎么用|如何使用|模式区别|magi(?:\s+|是|是什么)|who are you|what are you|what can you do|how to use|your role|capabilities)/i;

function includesAny(prompt: string, keywords: string[]): boolean {
  return keywords.some((keyword) => prompt.includes(keyword));
}

export function hasHardReadOnlyIntent(prompt: string): boolean {
  return HARD_READ_ONLY_PATTERN.test(prompt);
}

export function hasOrchestrationIntent(prompt: string): boolean {
  return ORCHESTRATION_PATTERN.test(prompt);
}

export function hasExplicitWorkerDispatchIntent(prompt: string): boolean {
  return EXPLICIT_WORKER_DISPATCH_PATTERN.test(prompt);
}

export function classifyRequest(prompt: string, mode: PlanMode): RequestClassification {
  const normalizedPrompt = prompt.toLowerCase();
  const readOnlyKeywordHit = includesAny(normalizedPrompt, READ_ONLY_KEYWORDS);
  const readOnlyPatternHit = READ_ONLY_PATTERN.test(prompt);
  const hardReadOnlyOverrideHit = hasHardReadOnlyIntent(prompt);
  const orchestrationIntentHit = hasOrchestrationIntent(prompt);
  const explicitWorkerDispatchHit = hasExplicitWorkerDispatchIntent(prompt);
  const writeKeywordHit = includesAny(normalizedPrompt, WRITE_KEYWORDS);
  const highImpactKeywordHit = includesAny(normalizedPrompt, HIGH_IMPACT_KEYWORDS);
  const workspaceKeywordHit = includesAny(normalizedPrompt, WORKSPACE_SCOPED_KEYWORDS);
  const fileLikeHit = FILE_LIKE_PATTERN.test(prompt);
  const pathLikeHit = PATH_LIKE_PATTERN.test(prompt);
  const codeReferenceHit = CODE_REFERENCE_PATTERN.test(prompt);
  const assistantMetaKeywordHit = includesAny(normalizedPrompt, ASSISTANT_META_KEYWORDS);
  const assistantMetaPatternHit = ASSISTANT_META_PATTERN.test(prompt);
  const conversationalKeywordHit = includesAny(normalizedPrompt, CONVERSATIONAL_KEYWORDS);
  const questionLikeHit = /[?？吗么]/.test(prompt);
  const hasReadOnlyIntent = readOnlyKeywordHit || readOnlyPatternHit;
  const hasWriteIntent = writeKeywordHit;
  const hasHighImpactIntent = highImpactKeywordHit;
  const hasWorkspaceScopedIntent = workspaceKeywordHit || fileLikeHit || pathLikeHit || codeReferenceHit;
  const hasAssistantMetaIntent = assistantMetaKeywordHit || assistantMetaPatternHit;
  const hasConversationalIntent = conversationalKeywordHit || questionLikeHit;
  const isShortConversationalTurn = prompt.trim().length <= 120;
  const requiresModification = !hardReadOnlyOverrideHit && (hasWriteIntent || hasHighImpactIntent);
  const readOnlyOrchestration = explicitWorkerDispatchHit && !requiresModification;
  const isReadOnlyAnalysis = !readOnlyOrchestration
    && !requiresModification
    && (hasReadOnlyIntent || hasWorkspaceScopedIntent || hardReadOnlyOverrideHit);

  const entryPath: RequestEntryPath = !requiresModification
    && !hasWorkspaceScopedIntent
    && isShortConversationalTurn
    && (hasAssistantMetaIntent || hasConversationalIntent)
    ? 'direct_response'
    : explicitWorkerDispatchHit
      ? 'task_execution'
    : isReadOnlyAnalysis
      ? 'lightweight_analysis'
      : 'task_execution';

  const entryPolicy: RequestEntryPolicy = entryPath === 'direct_response'
    ? {
        entryPath,
        includeThinking: false,
        includeToolCalls: false,
        historyMode: 'isolated',
      }
    : entryPath === 'lightweight_analysis'
      ? {
          entryPath,
          includeThinking: false,
          includeToolCalls: true,
          toolPolicy: buildAllowedToolsOnlyPolicy([...LIGHTWEIGHT_ANALYSIS_ALLOWED_TOOL_NAMES]),
          historyMode: 'isolated',
        }
      : {
          entryPath,
          includeThinking: true,
          // task_execution 路径必须启用工具：编排器的核心价值是通过 worker_dispatch 等工具调度任务，
          // 不注入工具则 LLM 只能以纯文本"叙述"工具调用，导致编排失败
          includeToolCalls: true,
          historyMode: 'session',
        };

  const decisionFactors = Array.from(new Set([
    readOnlyKeywordHit ? 'signal:read_only_keyword' : '',
    readOnlyPatternHit ? 'signal:read_only_pattern' : '',
    hardReadOnlyOverrideHit ? 'signal:hard_read_only_override' : '',
    orchestrationIntentHit ? 'signal:orchestration_intent' : '',
    explicitWorkerDispatchHit ? 'signal:explicit_worker_dispatch_intent' : '',
    writeKeywordHit ? 'signal:write_keyword' : '',
    highImpactKeywordHit ? 'signal:high_impact_keyword' : '',
    workspaceKeywordHit ? 'signal:workspace_keyword' : '',
    fileLikeHit ? 'signal:file_like_reference' : '',
    pathLikeHit ? 'signal:path_like_reference' : '',
    codeReferenceHit ? 'signal:code_reference' : '',
    assistantMetaKeywordHit ? 'signal:assistant_meta_keyword' : '',
    assistantMetaPatternHit ? 'signal:assistant_meta_pattern' : '',
    conversationalKeywordHit ? 'signal:conversational_keyword' : '',
    questionLikeHit ? 'signal:question_like' : '',
    isShortConversationalTurn ? 'signal:short_turn' : '',
    requiresModification ? 'inference:requires_modification=true' : 'inference:requires_modification=false',
    `decision:entry_path=${entryPath}`,
    `decision:history_mode=${entryPolicy.historyMode}`,
    `decision:include_tool_calls=${entryPolicy.includeToolCalls}`,
    `decision:include_thinking=${entryPolicy.includeThinking}`,
  ].filter(Boolean)));

  return {
    hasReadOnlyIntent,
    hasWriteIntent,
    hasHighImpactIntent,
    hasWorkspaceScopedIntent,
    hasAssistantMetaIntent,
    hasConversationalIntent,
    isShortConversationalTurn,
    requiresModification,
    entryPolicy,
    reason: entryPath === 'direct_response'
      ? '请求属于非任务型直接问答，应走轻量直答路径，避免编排历史、项目上下文和内部工具污染主线'
      : explicitWorkerDispatchHit
        ? '请求已明确要求 Worker 编排/派发，应进入唯一任务执行链并开放编排工具；即使当前目标是只读任务，也不能降级到轻量分析链'
      : entryPath === 'lightweight_analysis'
        ? '请求属于只读分析语义，应走轻量分析链路，允许按需使用只读工具，但不进入任务计划、Worker 调度和续跑主链'
        : '请求已进入任务执行语义，需要继续完成计划生成与后续调度决策',
    decisionFactors,
    classifierVersion: REQUEST_CLASSIFIER_VERSION,
  };
}
