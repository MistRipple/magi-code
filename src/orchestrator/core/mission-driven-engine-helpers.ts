import type { ContextManager } from '../../context/context-manager';
import type { ProjectKnowledgeBase } from '../../knowledge/project-knowledge-base';
import type { WisdomStorage } from '../wisdom';

export interface AdapterHistoryInfoSnapshot {
  messages: number;
  chars: number;
}

export interface OrchestratorContextPolicy {
  includeRecentTurns: boolean;
  totalTokens: number;
  localTurns: { min: number; max: number };
}

const KEY_INSTRUCTION_PATTERNS = [
  /不要|不能|必须|一定要|禁止|严禁/,
  /确认|同意|拒绝|取消|放弃/,
  /使用|采用|选择|决定/,
  /优先|首先|最重要/,
];

const USER_CONSTRAINT_PATTERNS = [
  /^(?:[-*]\s*)?(?:不要|不能|禁止|严禁|必须|一定要|务必|仅可|只允许)/,
  /(?:请务必|请确保|必须遵守|不得)/,
];

export function createWisdomStorage(
  contextManager: ContextManager,
  getProjectKnowledgeBase: () => ProjectKnowledgeBase | undefined,
): WisdomStorage {
  return {
    storeLearning: (learning: string, sourceAssignmentId: string) => {
      contextManager.addImportantContext(`[Learning:${sourceAssignmentId}] ${learning}`);
    },
    storeDecision: (decision: string, sourceAssignmentId: string) => {
      const decisionId = `decision-${sourceAssignmentId}-${Date.now().toString(36)}`;
      contextManager.addDecision(decisionId, decision, `来源 Assignment ${sourceAssignmentId}`);
    },
    storeWarning: (warning: string, sourceAssignmentId: string) => {
      contextManager.addPendingIssue(`[${sourceAssignmentId}] ${warning}`);
    },
    storeSignificantLearning: (learning: string, context: string) => {
      const knowledgeBase = getProjectKnowledgeBase();
      if (knowledgeBase) {
        knowledgeBase.addLearning(learning, context);
        return;
      }
      contextManager.addImportantContext(`[Knowledge] ${learning} (${context})`);
    },
  };
}

export function resolveOrchestratorContextPolicy(
  historyInfo?: AdapterHistoryInfoSnapshot,
): OrchestratorContextPolicy {
  if (!historyInfo || historyInfo.messages === 0) {
    return {
      includeRecentTurns: true,
      totalTokens: 7600,
      localTurns: { min: 1, max: 8 },
    };
  }

  if (historyInfo.messages <= 6 || historyInfo.chars <= 12000) {
    return {
      includeRecentTurns: true,
      totalTokens: 6200,
      localTurns: { min: 1, max: 4 },
    };
  }

  if (historyInfo.messages <= 14 && historyInfo.chars <= 40000) {
    return {
      includeRecentTurns: false,
      totalTokens: 7600,
      localTurns: { min: 1, max: 6 },
    };
  }

  return {
    includeRecentTurns: false,
    totalTokens: 6400,
    localTurns: { min: 1, max: 4 },
  };
}

export function isKeyInstruction(content: string): boolean {
  return KEY_INSTRUCTION_PATTERNS.some(pattern => pattern.test(content));
}

export function extractPrimaryIntent(content: string): string {
  const trimmed = content.trim();
  if (trimmed.length <= 100) {
    return trimmed;
  }

  const breakPoint = trimmed.substring(0, 100).lastIndexOf('。');
  if (breakPoint > 30) {
    return trimmed.substring(0, breakPoint + 1);
  }
  return `${trimmed.substring(0, 100)}...`;
}

export function extractUserConstraints(content: string): string[] {
  const constraints = content
    .split(/\n+/)
    .map(line => line.trim())
    .filter(line => line.length > 0)
    .filter(line => USER_CONSTRAINT_PATTERNS.some(pattern => pattern.test(line)))
    .map(line => (line.length > 120 ? `${line.substring(0, 120)}...` : line));

  return Array.from(new Set(constraints)).slice(0, 5);
}
