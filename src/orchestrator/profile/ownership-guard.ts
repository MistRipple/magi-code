import { t } from '../../i18n';
import { DomainDetector, type OwnershipDomain, OWNERSHIP_DOMAIN_CATEGORIES } from './domain-detector';
import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';

export interface DispatchOwnershipAdvisory {
  degraded: boolean;
  resolvedCategory?: string;
  routingReasonPatch?: string;
  warningDetail?: string;
  rejectionError?: string;
}

export interface DispatchOwnershipAdvisoryInput {
  category: string;
  taskTitle: string;
  goal: string;
  acceptance: string[];
  constraints: string[];
  context: string[];
  dependsOn?: string[];
  userPrompt?: string;
}

export interface ResolvedDispatchPhaseAdvisoryInput {
  category: string;
  taskTitle: string;
  goal: string;
  acceptance: string[];
  constraints: string[];
  context: string[];
  userPrompt?: string;
  hasResolvedHistoricalDependencies: boolean;
  hasInBatchDependencies: boolean;
  hasActiveFeatureDomainEntriesInBatch: boolean;
}

export interface SplitTodoOwnershipInput {
  assignmentCategory: string;
  subtasks: Array<{
    content: string;
    reasoning: string;
    expectedOutput: string;
  }>;
}

export const FALLBACK_ROUTING_CATEGORIES = new Set(['implement', 'general', 'simple']);
export const TODO_SPLIT_STRICT_OWNERSHIP_CATEGORIES = new Set<'frontend' | 'backend'>(['frontend', 'backend']);
const OWNERSHIP_CATEGORY_SET = new Set<OwnershipDomain>(OWNERSHIP_DOMAIN_CATEGORIES);
const AUXILIARY_OWNERSHIP_CATEGORIES = new Set<OwnershipDomain>(['test', 'document', 'data_analysis']);
const IMPLEMENTATION_BOOTSTRAP_SIGNAL = /(新增|新建|开发|搭建|创建|从0到1|feature|功能点|crud)/i;
const INTEGRATION_FOLLOWUP_SIGNAL = /(现有|已有|已完成|已存在|既有|存量|当前|排查|修复|问题|故障|对齐|兼容)/i;
const OWNERSHIP_PROMPT_META_PATTERNS = [
  /必须调用\s*worker_dispatch(?:\s*与\s*worker_wait)?/ig,
  /(?:随后|然后|再)\s*调用?\s*worker_wait/ig,
  /本轮只做[^。！？；;\n]*(?:分析|编排|规划)[^。！？；;\n]*/ig,
  /仅做[^。！？；;\n]*(?:分析|编排|规划)[^。！？；;\n]*/ig,
  /只做[^。！？；;\n]*(?:分析|编排|规划)[^。！？；;\n]*/ig,
  /只读分析/ig,
  /任务编排/ig,
  /不修改代码|不要修改代码|禁止修改代码/ig,
] as const;
const OWNERSHIP_GOAL_PREFIX = /(?:用户目标|目标|需求)[:：]\s*/i;

export class OwnershipGuard {
  constructor(
    private readonly domainDetector = new DomainDetector(),
  ) {}

  evaluateDispatchAdvisory(input: DispatchOwnershipAdvisoryInput): DispatchOwnershipAdvisory {
    const taskTextParts = [
      input.taskTitle,
      input.goal,
      ...input.acceptance,
      ...input.constraints,
      ...input.context,
    ];
    const taskDetection = this.domainDetector.detectFromTextParts(taskTextParts);
    const normalizedUserPrompt = this.normalizeUserPromptForOwnership(input.userPrompt);
    const promptDetection = normalizedUserPrompt
      ? this.domainDetector.detectFromTextParts([normalizedUserPrompt])
      : { matchedDomains: [], splitBoundaryDomains: [] };

    const explicitCategoryAdvisory = this.evaluateExplicitCategoryConsistency(
      input.category,
      taskTextParts,
      taskDetection,
      promptDetection,
      input.dependsOn,
      normalizedUserPrompt || undefined,
    );
    if (explicitCategoryAdvisory?.rejectionError) {
      return explicitCategoryAdvisory;
    }

    const explicitOwnershipCategory = OWNERSHIP_CATEGORY_SET.has(input.category as OwnershipDomain)
      ? input.category as OwnershipDomain
      : null;
    if (explicitOwnershipCategory) {
      return {
        degraded: explicitCategoryAdvisory?.degraded === true,
        resolvedCategory: explicitOwnershipCategory,
        ...(explicitCategoryAdvisory?.routingReasonPatch
          ? { routingReasonPatch: explicitCategoryAdvisory.routingReasonPatch }
          : {}),
        ...(explicitCategoryAdvisory?.warningDetail
          ? { warningDetail: explicitCategoryAdvisory.warningDetail }
          : {}),
      };
    }

    const inferredOwnershipAdvisory = this.evaluateInferredOwnershipCategory({
      category: input.category,
      taskDetection,
      promptDetection,
    });
    if (inferredOwnershipAdvisory) {
      return inferredOwnershipAdvisory;
    }

    return {
      degraded: explicitCategoryAdvisory?.degraded === true,
      resolvedCategory: input.category,
      ...(explicitCategoryAdvisory?.routingReasonPatch
        ? { routingReasonPatch: explicitCategoryAdvisory.routingReasonPatch }
        : {}),
      ...(explicitCategoryAdvisory?.warningDetail
        ? { warningDetail: explicitCategoryAdvisory.warningDetail }
        : {}),
    };
  }

  evaluateResolvedDispatchPhaseAdvisory(
    input: ResolvedDispatchPhaseAdvisoryInput,
  ): DispatchOwnershipAdvisory | undefined {
    if (input.category !== 'integration') {
      return undefined;
    }

    if (input.hasInBatchDependencies || input.hasActiveFeatureDomainEntriesInBatch) {
      return {
        degraded: true,
        rejectionError: t('dispatch.errors.integrationPhaseMustRunLater'),
      };
    }

    if (input.hasResolvedHistoricalDependencies) {
      return undefined;
    }

    const taskTextParts = [
      input.taskTitle,
      input.goal,
      ...input.acceptance,
      ...input.constraints,
      ...input.context,
    ];
    const taskDetection = this.domainDetector.detectFromTextParts(taskTextParts);
    const normalizedUserPrompt = this.normalizeUserPromptForOwnership(input.userPrompt);
    const promptDetection = normalizedUserPrompt
      ? this.domainDetector.detectFromTextParts([normalizedUserPrompt])
      : { matchedDomains: [], splitBoundaryDomains: [] };

    return this.evaluateIntegrationStageConsistency({
      ownershipCategory: 'integration',
      taskTextParts,
      taskDetection,
      promptDetection,
      dependsOn: undefined,
      userPrompt: normalizedUserPrompt || undefined,
    });
  }

  private resolveFallbackAdvisoryDomains(
    detection: { matchedDomains: OwnershipDomain[]; splitBoundaryDomains: OwnershipDomain[] },
  ): OwnershipDomain[] {
    if (detection.splitBoundaryDomains.length > 0) {
      return [...detection.splitBoundaryDomains];
    }
    if (detection.matchedDomains.length === 1) {
      return [...detection.matchedDomains];
    }
    if (
      detection.matchedDomains.length > 1
      && detection.matchedDomains.some((domain) => !AUXILIARY_OWNERSHIP_CATEGORIES.has(domain))
    ) {
      return detection.matchedDomains.filter((domain) => !AUXILIARY_OWNERSHIP_CATEGORIES.has(domain));
    }
    return [];
  }

  private evaluateExplicitCategoryConsistency(
    category: string,
    taskTextParts: string[],
    taskDetection: { matchedDomains: OwnershipDomain[]; splitBoundaryDomains: OwnershipDomain[] },
    promptDetection: { matchedDomains: OwnershipDomain[]; splitBoundaryDomains: OwnershipDomain[] },
    dependsOn: string[] | undefined,
    userPrompt?: string,
  ): DispatchOwnershipAdvisory | undefined {
    const ownershipCategory = OWNERSHIP_CATEGORY_SET.has(category as OwnershipDomain)
      ? category as OwnershipDomain
      : null;
    if (!ownershipCategory) {
      return undefined;
    }
    const promptHasBoundaryScope = promptDetection.splitBoundaryDomains.length >= 1;
    const promptHasCategorySignal = typeof userPrompt === 'string'
      && userPrompt.trim().length > 0
      && this.matchesCategorySignal(category, [userPrompt]);

    if (promptHasBoundaryScope && !promptHasCategorySignal) {
      return {
        degraded: true,
        rejectionError: t('dispatch.errors.categoryOwnershipMismatch', {
          category,
          taskDomains: taskDetection.matchedDomains.join('/') || 'none',
          promptDomains: promptDetection.matchedDomains.join('/') || 'none',
        }),
      };
    }

    const integrationStageAdvisory = this.evaluateIntegrationStageConsistency({
      ownershipCategory,
      taskTextParts,
      taskDetection,
      promptDetection,
      dependsOn,
      userPrompt,
    });
    if (integrationStageAdvisory) {
      return integrationStageAdvisory;
    }

    if (taskDetection.matchedDomains.length > 0 && !taskDetection.matchedDomains.includes(ownershipCategory)) {
      return {
        degraded: true,
        rejectionError: t('dispatch.errors.categoryOwnershipMismatch', {
          category,
          taskDomains: taskDetection.matchedDomains.join('/') || 'none',
          promptDomains: promptDetection.matchedDomains.join('/') || 'none',
        }),
      };
    }

    if (
      AUXILIARY_OWNERSHIP_CATEGORIES.has(ownershipCategory)
      && taskDetection.splitBoundaryDomains.length > 1
    ) {
      return {
        degraded: true,
        rejectionError: t('dispatch.errors.categoryOwnershipMismatch', {
          category,
          taskDomains: taskDetection.matchedDomains.join('/') || 'none',
          promptDomains: promptDetection.matchedDomains.join('/') || 'none',
        }),
      };
    }

    if (
      promptHasBoundaryScope
      && AUXILIARY_OWNERSHIP_CATEGORIES.has(ownershipCategory)
      && !promptDetection.matchedDomains.includes(ownershipCategory)
      && taskDetection.splitBoundaryDomains.length === 0
    ) {
      return {
        degraded: true,
        rejectionError: t('dispatch.errors.categoryOwnershipMismatch', {
          category,
          taskDomains: taskDetection.matchedDomains.join('/') || 'none',
          promptDomains: promptDetection.matchedDomains.join('/') || 'none',
        }),
      };
    }

    if (
      promptHasBoundaryScope
      && ownershipCategory === 'integration'
      && !promptDetection.matchedDomains.includes('integration')
      && taskDetection.splitBoundaryDomains.length >= 1
    ) {
      return {
        degraded: true,
        rejectionError: t('dispatch.errors.categoryOwnershipMismatch', {
          category,
          taskDomains: taskDetection.matchedDomains.join('/') || 'none',
          promptDomains: promptDetection.matchedDomains.join('/') || 'none',
        }),
      };
    }

    return undefined;
  }

  private evaluateInferredOwnershipCategory(input: {
    category: string;
    taskDetection: { matchedDomains: OwnershipDomain[]; splitBoundaryDomains: OwnershipDomain[] };
    promptDetection: { matchedDomains: OwnershipDomain[]; splitBoundaryDomains: OwnershipDomain[] };
  }): DispatchOwnershipAdvisory | undefined {
    const taskSplitDomains = input.taskDetection.splitBoundaryDomains;
    if (taskSplitDomains.length > 1) {
      return {
        degraded: true,
        rejectionError: t(
          FALLBACK_ROUTING_CATEGORIES.has(input.category)
            ? 'dispatch.errors.ownershipSplitRequired'
            : 'dispatch.errors.categoryOwnershipSplitRequired',
          {
            category: input.category,
            domains: taskSplitDomains.join('/'),
          },
        ),
      };
    }

    const taskDomains = this.resolveFallbackAdvisoryDomains(input.taskDetection);
    if (taskDomains.length === 1) {
      return this.buildResolvedCategoryAdvisory(input.category, taskDomains[0]);
    }

    const promptSplitDomains = input.promptDetection.splitBoundaryDomains;
    if (promptSplitDomains.length > 1) {
      return {
        degraded: true,
        rejectionError: t('dispatch.errors.categoryOwnershipMismatch', {
          category: input.category,
          taskDomains: input.taskDetection.matchedDomains.join('/') || 'none',
          promptDomains: input.promptDetection.matchedDomains.join('/') || 'none',
        }),
      };
    }

    const promptDomains = this.resolveFallbackAdvisoryDomains(input.promptDetection);
    if (promptDomains.length === 1) {
      return this.buildResolvedCategoryAdvisory(input.category, promptDomains[0]);
    }

    return undefined;
  }

  private buildResolvedCategoryAdvisory(
    fromCategory: string,
    toCategory: OwnershipDomain,
  ): DispatchOwnershipAdvisory {
    return {
      degraded: true,
      resolvedCategory: toCategory,
      routingReasonPatch: t('dispatch.notify.ownershipInferredSingleDomainAutoAdjustReason', {
        fromCategory,
        toCategory,
        domains: toCategory,
      }),
    };
  }

  private evaluateIntegrationStageConsistency(input: {
    ownershipCategory: OwnershipDomain;
    taskTextParts: string[];
    taskDetection: { matchedDomains: OwnershipDomain[]; splitBoundaryDomains: OwnershipDomain[] };
    promptDetection: { matchedDomains: OwnershipDomain[]; splitBoundaryDomains: OwnershipDomain[] };
    dependsOn?: string[];
    userPrompt?: string;
  }): DispatchOwnershipAdvisory | undefined {
    if (input.ownershipCategory !== 'integration') {
      return undefined;
    }

    if (Array.isArray(input.dependsOn) && input.dependsOn.length > 0) {
      return undefined;
    }

    const taskTouchesBuildDomains = this.hasFrontendBackendBoundary(input.taskDetection.splitBoundaryDomains);
    const promptTouchesBuildDomains = this.hasFrontendBackendBoundary(input.promptDetection.splitBoundaryDomains);
    if (!taskTouchesBuildDomains && !promptTouchesBuildDomains) {
      return undefined;
    }

    const taskText = this.composeText(input.taskTextParts);
    const promptText = this.composeText(typeof input.userPrompt === 'string' ? [input.userPrompt] : []);
    const looksLikeFeatureBootstrap =
      (taskTouchesBuildDomains && IMPLEMENTATION_BOOTSTRAP_SIGNAL.test(taskText))
      || (promptTouchesBuildDomains && IMPLEMENTATION_BOOTSTRAP_SIGNAL.test(promptText));
    const looksLikeExistingIntegrationFollowup =
      INTEGRATION_FOLLOWUP_SIGNAL.test(taskText) || INTEGRATION_FOLLOWUP_SIGNAL.test(promptText);

    if (!looksLikeFeatureBootstrap && looksLikeExistingIntegrationFollowup) {
      return undefined;
    }

    return {
      degraded: true,
      rejectionError: t('dispatch.errors.integrationPhaseRequired'),
    };
  }

  private hasFrontendBackendBoundary(domains: OwnershipDomain[]): boolean {
    return domains.includes('frontend') || domains.includes('backend');
  }

  private composeText(parts: string[]): string {
    return parts
      .filter((part): part is string => typeof part === 'string' && part.trim().length > 0)
      .join('\n')
      .toLowerCase();
  }

  private matchesCategorySignal(category: string, parts: string[]): boolean {
    const definition = CATEGORY_DEFINITIONS[category];
    if (!definition || !Array.isArray(definition.keywords) || definition.keywords.length === 0) {
      return false;
    }

    const text = parts
      .filter((part): part is string => typeof part === 'string' && part.trim().length > 0)
      .join('\n')
      .toLowerCase();
    if (!text) {
      return false;
    }

    return definition.keywords.some((pattern) => {
      try {
        return new RegExp(pattern, 'i').test(text);
      } catch {
        return false;
      }
    });
  }

  private normalizeUserPromptForOwnership(userPrompt?: string): string {
    if (typeof userPrompt !== 'string' || userPrompt.trim().length === 0) {
      return '';
    }

    let normalized = userPrompt.trim();
    const goalPrefixMatch = normalized.match(OWNERSHIP_GOAL_PREFIX);
    if (goalPrefixMatch?.index !== undefined) {
      normalized = normalized.slice(goalPrefixMatch.index + goalPrefixMatch[0].length);
    }

    for (const pattern of OWNERSHIP_PROMPT_META_PATTERNS) {
      normalized = normalized.replace(pattern, ' ');
    }

    return normalized
      .replace(/\s+/g, ' ')
      .trim();
  }

  evaluateSplitTodoOwnership(input: SplitTodoOwnershipInput): string | undefined {
    if (!TODO_SPLIT_STRICT_OWNERSHIP_CATEGORIES.has(input.assignmentCategory as 'frontend' | 'backend')) {
      return undefined;
    }

    const assignmentCategory = input.assignmentCategory as 'frontend' | 'backend';
    const conflictingDomain = assignmentCategory === 'frontend' ? 'backend' : 'frontend';
    const violatingIndexes: number[] = [];

    input.subtasks.forEach((subtask, index) => {
      const detection = this.domainDetector.detectFromTextParts([
        subtask.content,
        subtask.reasoning,
        subtask.expectedOutput,
      ]);
      if (detection.splitBoundaryDomains.length !== 1) {
        return;
      }
      if (detection.splitBoundaryDomains[0] === conflictingDomain) {
        violatingIndexes.push(index + 1);
      }
    });

    if (violatingIndexes.length === 0) {
      return undefined;
    }

    return t('dispatch.errors.splitTodoOwnershipViolation', {
      assignmentCategory,
      conflictingDomain,
      subtaskIndexes: violatingIndexes.join(', '),
    });
  }
}
