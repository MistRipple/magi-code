import type { ToolCall } from '../llm/types';
import type { InstructionSkillDefinition } from './skills-manager';
import { getModeConstraints, type TaskMode } from '../orchestrator/profile/task-taxonomy';

export type ToolPolicySource = 'request' | 'mode' | 'skill' | 'composed';

export interface EffectiveToolPolicy {
  schemaVersion: 'tool-policy.v1';
  source: ToolPolicySource;
  allowedToolNames?: string[];
  forbiddenToolNames?: string[];
  readOnly?: boolean;
  /**
   * 多组允许路径模式。
   * 每一组内部是“任一匹配即可”，多组之间是“必须每组都命中一条”。
   * 这样可表达多个来源策略叠加后的收窄语义。
   */
  allowedFilePatternGroups?: string[][];
  forbiddenFilePatterns?: string[];
  restrictUnknownExternalTools?: boolean;
  activeInstructionSkillName?: string;
}

const BUILTIN_WRITE_TOOL_NAMES = new Set<string>([
  'file_create',
  'file_edit',
  'file_insert',
  'file_remove',
]);

const BUILTIN_ORCHESTRATION_TOOL_NAMES = new Set<string>([
  'worker_dispatch',
  'worker_send_message',
  'worker_wait',
  'context_compact',
]);

function normalizeNameList(values?: string[]): string[] | undefined {
  const normalized = Array.from(new Set(
    (values || [])
      .map((value) => value.trim())
      .filter(Boolean),
  ));
  return normalized.length > 0 ? normalized : undefined;
}

function normalizePatternList(values?: string[]): string[] | undefined {
  const normalized = Array.from(new Set(
    (values || [])
      .map((value) => value.trim())
      .filter(Boolean),
  ));
  return normalized.length > 0 ? normalized : undefined;
}

function normalizeAllowedPatternGroups(values?: string[][]): string[][] | undefined {
  if (!values || values.length === 0) {
    return undefined;
  }
  const groups = values
    .map((group) => normalizePatternList(group))
    .filter((group): group is string[] => Boolean(group && group.length > 0));
  return groups.length > 0 ? groups : undefined;
}

function serializePattern(pattern: RegExp): string {
  return pattern.source;
}

export function buildAllowedToolsOnlyPolicy(
  allowedToolNames: string[],
  source: ToolPolicySource = 'request',
): EffectiveToolPolicy {
  return {
    schemaVersion: 'tool-policy.v1',
    source,
    allowedToolNames: normalizeNameList(allowedToolNames),
  };
}

export function buildModeToolPolicy(mode: TaskMode): EffectiveToolPolicy | undefined {
  const constraints = getModeConstraints(mode);
  const allowedToolNames = normalizeNameList(constraints.allowedTools);
  const forbiddenToolNames = normalizeNameList(constraints.forbiddenTools);
  const allowedFilePatterns = constraints.allowedFilePatterns?.map(serializePattern);
  const forbiddenFilePatterns = constraints.forbiddenFilePatterns?.map(serializePattern);
  const readOnly = constraints.readOnly === true;
  const hasConstraints = readOnly
    || Boolean(allowedToolNames?.length)
    || Boolean(forbiddenToolNames?.length)
    || Boolean(allowedFilePatterns?.length)
    || Boolean(forbiddenFilePatterns?.length);

  if (!hasConstraints) {
    return undefined;
  }

  return {
    schemaVersion: 'tool-policy.v1',
    source: 'mode',
    ...(allowedToolNames ? { allowedToolNames } : {}),
    ...(forbiddenToolNames ? { forbiddenToolNames } : {}),
    ...(readOnly ? { readOnly: true } : {}),
    ...(allowedFilePatterns ? { allowedFilePatternGroups: [allowedFilePatterns] } : {}),
    ...(forbiddenFilePatterns ? { forbiddenFilePatterns } : {}),
    /**
     * mode 约束下外部工具一律走保守策略：
     * 没有明确允许集合时，不放开未知副作用的外部工具。
     */
    restrictUnknownExternalTools: true,
  };
}

export function buildInstructionSkillToolPolicy(
  skill: Pick<InstructionSkillDefinition, 'name' | 'allowedTools'>,
): EffectiveToolPolicy {
  const allowedToolNames = normalizeNameList(skill.allowedTools);
  return {
    schemaVersion: 'tool-policy.v1',
    source: 'skill',
    ...(allowedToolNames ? { allowedToolNames } : {}),
    activeInstructionSkillName: skill.name,
    /**
     * Skill 激活后的工具边界必须收敛到显式声明的能力集合；
     * 未声明时仍保持保守的外部工具策略，避免未知工具逃逸。
     */
    restrictUnknownExternalTools: true,
  };
}

function intersectNameLists(left?: string[], right?: string[]): string[] | undefined {
  if (!left?.length) {
    return right ? [...right] : undefined;
  }
  if (!right?.length) {
    return [...left];
  }
  const rightSet = new Set(right);
  const intersection = left.filter((item) => rightSet.has(item));
  return intersection.length > 0 ? intersection : [];
}

export function mergeToolPolicies(
  policies: Array<EffectiveToolPolicy | undefined | null>,
): EffectiveToolPolicy | undefined {
  const normalizedPolicies = policies.filter((policy): policy is EffectiveToolPolicy => Boolean(policy));
  if (normalizedPolicies.length === 0) {
    return undefined;
  }

  let allowedToolNames: string[] | undefined;
  const forbiddenToolNames: string[] = [];
  const allowedFilePatternGroups: string[][] = [];
  const forbiddenFilePatterns: string[] = [];
  let readOnly = false;
  let restrictUnknownExternalTools = false;
  let activeInstructionSkillName: string | undefined;

  for (const policy of normalizedPolicies) {
    allowedToolNames = intersectNameLists(allowedToolNames, normalizeNameList(policy.allowedToolNames));
    if (policy.forbiddenToolNames?.length) {
      forbiddenToolNames.push(...policy.forbiddenToolNames);
    }
    if (policy.allowedFilePatternGroups?.length) {
      allowedFilePatternGroups.push(...policy.allowedFilePatternGroups);
    }
    if (policy.forbiddenFilePatterns?.length) {
      forbiddenFilePatterns.push(...policy.forbiddenFilePatterns);
    }
    if (policy.readOnly) {
      readOnly = true;
    }
    if (policy.restrictUnknownExternalTools) {
      restrictUnknownExternalTools = true;
    }
    if (policy.activeInstructionSkillName) {
      activeInstructionSkillName = policy.activeInstructionSkillName;
    }
  }

  return {
    schemaVersion: 'tool-policy.v1',
    source: normalizedPolicies.length === 1 ? normalizedPolicies[0].source : 'composed',
    ...(allowedToolNames ? { allowedToolNames } : {}),
    ...(forbiddenToolNames.length > 0 ? { forbiddenToolNames: normalizeNameList(forbiddenToolNames) } : {}),
    ...(allowedFilePatternGroups.length > 0
      ? { allowedFilePatternGroups: normalizeAllowedPatternGroups(allowedFilePatternGroups) }
      : {}),
    ...(forbiddenFilePatterns.length > 0
      ? { forbiddenFilePatterns: normalizePatternList(forbiddenFilePatterns) }
      : {}),
    ...(readOnly ? { readOnly: true } : {}),
    ...(restrictUnknownExternalTools ? { restrictUnknownExternalTools: true } : {}),
    ...(activeInstructionSkillName ? { activeInstructionSkillName } : {}),
  };
}

export function isToolBlockedByPolicy(
  toolName: string,
  policy?: EffectiveToolPolicy,
): boolean {
  if (!policy) {
    return false;
  }
  if (policy.allowedToolNames?.length && !policy.allowedToolNames.includes(toolName)) {
    return true;
  }
  if (policy.forbiddenToolNames?.length && policy.forbiddenToolNames.includes(toolName)) {
    return true;
  }
  return false;
}

export function hasFileScopeRestrictions(policy?: EffectiveToolPolicy): boolean {
  return Boolean(policy?.allowedFilePatternGroups?.length || policy?.forbiddenFilePatterns?.length);
}

export function isBuiltinWriteToolCall(toolCall: Pick<ToolCall, 'name' | 'arguments'>): boolean {
  if (BUILTIN_WRITE_TOOL_NAMES.has(toolCall.name)) {
    return true;
  }
  if (toolCall.name !== 'shell') {
    return false;
  }
  return toolCall.arguments?.may_modify_files === true;
}

export function isOrchestrationToolName(toolName: string): boolean {
  return BUILTIN_ORCHESTRATION_TOOL_NAMES.has(toolName);
}
