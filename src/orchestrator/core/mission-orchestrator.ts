/**
 * Mission Orchestrator - 任务编排核心
 *
 * 核心职责：
 * - 接收用户请求，创建 Mission
 * - 协调 Mission 的规划流程
 * - 管理 Mission 生命周期
 * - 协调多 Worker 协作
 */

import { EventEmitter } from 'events';
import fs from 'fs';
import path from 'path';
import { WorkerSlot } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector, TaskStructuredInfo } from '../profile/guidance-injector';
import { AssignmentResolver } from '../profile/assignment-resolver';
import { ProfileAwareReviewer } from '../review/profile-aware-reviewer';
import {
  VerificationRunner,
  VerificationResult,
  VerificationConfig,
} from '../verification-runner';
import {
  IntentGate,
  IntentGateResult,
  IntentHandlerMode,
  IntentDecider,
} from '../intent-gate';
import { SnapshotManager } from '../../snapshot-manager';
import { ContextManager } from '../../context/context-manager';
import { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { logger, LogCategory } from '../../logging';
import { LLMConfigLoader } from '../../llm/config';
import {
  MissionStorageManager,
  ContractManager,
  AssignmentManager,
  Mission,
  Contract,
  Assignment,
  CreateMissionParams,
  VerificationSpec,
  AcceptanceCriterion,
} from '../mission';
import { AutonomousWorker, AutonomousExecutionResult } from '../worker';
import { TodoManager } from '../../todo';
import type { UnifiedTodo } from '../../todo/types';
import type { ReportCallback } from '../protocols/worker-report';
import { TokenUsage } from '../../types/agent-types';

/**
 * Mission 创建结果
 */
export interface MissionCreationResult {
  mission: Mission;
  contracts: Contract[];
  assignments: Assignment[];
}

/**
 * 执行选项（从 MissionExecutor 合并）
 */
export interface ExecutionOptions {
  /** 工作目录 */
  workingDirectory: string;
  /** 超时时间（毫秒） */
  timeout?: number;
  /** 项目上下文 */
  projectContext?: string;
  /** 并行执行 */
  parallel?: boolean;
  /** 使用 Wave 并行分组执行 */
  useWaveExecution?: boolean;
  /** 并行规划（默认 true） */
  parallelPlanning?: boolean;
  /** 阻塞超时时间（毫秒），超时后跳过阻塞项 */
  blockingTimeout?: number;
  /** 阻塞检查间隔（毫秒） */
  blockingCheckInterval?: number;
  /** 输出回调 */
  onOutput?: (workerId: WorkerSlot, output: string) => void;
  /** 进度回调 */
  onProgress?: (progress: ExecutionProgress) => void;
  /** 阻塞回调 */
  onBlocked?: (blockedItem: any) => void;
  /** 解除阻塞回调 */
  onUnblocked?: (blockedItem: any) => void;
  /** Worker 汇报回调 */
  onReport?: ReportCallback;
  /** 汇报超时(ms) */
  reportTimeout?: number;
  /** 获取补充指令（在决策点注入） */
  getSupplementaryInstructions?: (workerId: WorkerSlot) => string[];
}

/**
 * 执行进度（从 MissionExecutor 合并）
 */
export interface ExecutionProgress {
  missionId: string;
  phase: 'planning' | 'executing' | 'reviewing' | 'completed';
  totalAssignments: number;
  completedAssignments: number;
  blockedAssignments: number;
  currentAssignment?: {
    id: string;
    workerId: WorkerSlot;
    progress: number;
  };
  blockedItems: any[];
  overallProgress: number;
}

/**
 * 执行结果（从 MissionExecutor 合并）
 */
export interface ExecutionResult {
  mission: Mission;
  success: boolean;
  assignmentResults: Map<string, AutonomousExecutionResult>;
  contractVerifications: Map<string, boolean>;
  blockedItems: any[];
  resolvedBlockings: any[];
  errors: string[];
  duration: number;
  /** 聚合的 Token 使用统计 */
  tokenUsage?: TokenUsage;
  hasPendingApprovals?: boolean;
}

/**
 * 规划选项
 */
export interface PlanningOptions {
  /** 参与者列表（如果不指定，自动选择） */
  participants?: WorkerSlot[];
  /** 项目上下文 */
  projectContext?: string;
  /** 是否需要用户确认 */
  requireApproval?: boolean;
}

/**
 * 验证结果
 */
export interface MissionVerificationResult {
  /** 是否验证通过 */
  passed: boolean;
  /** 验收标准状态 */
  criteriaStatus: Array<{
    criterionId: string;
    description: string;
    passed: boolean;
    reason?: string;
  }>;
  /** 技术验证结果 */
  technicalVerification?: VerificationResult;
  /** 契约验证结果 */
  contractsVerified: boolean;
  /** 契约违反详情 */
  contractViolations: string[];
  /** 总结 */
  summary: string;
}

/**
 * Mission 总结
 */
export interface MissionSummary {
  /** Mission ID */
  missionId: string;
  /** 目标 */
  goal: string;
  /** 成功状态 */
  success: boolean;
  /** 执行时长 */
  duration: number;
  /** 修改的文件 */
  modifiedFiles: string[];
  /** 完成的 Todo 数量 */
  completedTodos: number;
  /** 失败的 Todo 数量 */
  failedTodos: number;
  /** 跳过的 Todo 数量 */
  skippedTodos: number;
  /** 恢复尝试次数 */
  recoveryAttempts: number;
  /** Worker 贡献 */
  workerContributions: Record<WorkerSlot, {
    assignmentCount: number;
    completedTodos: number;
    failedTodos: number;
  }>;
  /** 关键成就 */
  keyAchievements: string[];
  /** 遗留问题 */
  remainingIssues: string[];
  /** 建议后续步骤 */
  suggestedNextSteps: string[];
}

/**
 * MissionOrchestrator - 任务编排核心
 */
export class MissionOrchestrator extends EventEmitter {
  private storage: MissionStorageManager;
  private contractManager: ContractManager;
  private assignmentManager: AssignmentManager;
  private reviewer: ProfileAwareReviewer;
  private verificationRunner?: VerificationRunner;
  private intentGate?: IntentGate;
  private snapshotManager?: SnapshotManager;
  private contextManager: ContextManager;
  private adapterFactory: IAdapterFactory;

  // 项目知识库
  private projectKnowledgeBase?: import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase;

  // 规划结果缓存（基于 prompt hash）
  private planningCache: Map<string, { mission: Mission; timestamp: number }> = new Map();
  private readonly CACHE_TTL_MS = 5 * 60 * 1000; // 5 分钟缓存过期

  private assignmentResolver: AssignmentResolver;

  // 执行相关属性（从 MissionExecutor 合并）
  private workers: Map<WorkerSlot, AutonomousWorker> = new Map();
  private todoManager?: TodoManager;
  private currentMissionId: string | null = null;

  constructor(
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector,
    adapterFactory: IAdapterFactory,
    contextManager: ContextManager,
    storage?: MissionStorageManager,
    private workspaceRoot?: string,
    snapshotManager?: SnapshotManager,
  ) {
    super();
    this.adapterFactory = adapterFactory;
    this.contextManager = contextManager;
    this.snapshotManager = snapshotManager;
    this.storage = storage || new MissionStorageManager();
    this.contractManager = new ContractManager();
    this.assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    this.reviewer = new ProfileAwareReviewer(profileLoader);
    this.assignmentResolver = new AssignmentResolver(profileLoader.getAssignmentLoader());

    if (workspaceRoot) {
      this.verificationRunner = new VerificationRunner(workspaceRoot);
    }

    this.setupStorageListeners();
  }

  /**
   * 获取 SnapshotManager
   */
  getSnapshotManager(): SnapshotManager | undefined {
    return this.snapshotManager;
  }

  /**
   * 获取 ContextManager
   */
  getContextManager(): ContextManager | undefined {
    return this.contextManager;
  }

  /**
   * 设置项目知识库
   */
  setKnowledgeBase(knowledgeBase: import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase): void {
    this.projectKnowledgeBase = knowledgeBase;
    logger.info('任务编排器.知识库.已设置', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 获取项目知识库上下文
   */
  private getProjectContext(maxTokens: number = 600): string {
    if (!this.projectKnowledgeBase) {
      return '';
    }
    return this.projectKnowledgeBase.getProjectContext(maxTokens);
  }

  /**
   * 获取相关的 ADRs
   */
  private getRelevantADRs(userPrompt: string): string {
    if (!this.projectKnowledgeBase) {
      return '';
    }

    const adrs = this.projectKnowledgeBase.getADRs({ status: 'accepted' });
    if (adrs.length === 0) {
      return '';
    }

    // 简单的关键词匹配
    const keywords = userPrompt.toLowerCase().split(/\s+/);
    const relevantADRs = adrs.filter(adr => {
      const adrText = `${adr.title} ${adr.context} ${adr.decision}`.toLowerCase();
      return keywords.some(keyword => keyword.length > 2 && adrText.includes(keyword));
    }).slice(0, 2); // 最多2个，避免上下文过长

    if (relevantADRs.length === 0) {
      return '';
    }

    const parts: string[] = [];
    relevantADRs.forEach(adr => {
      parts.push(`[ADR-${adr.id}] ${adr.title}`);
      parts.push(`决策: ${adr.decision}`);
      if (adr.consequences) {
        parts.push(`影响: ${adr.consequences}`);
      }
    });

    return parts.join('\n');
  }

  /**
   * 初始化上下文管理器
   */
  async initializeContext(sessionId: string, sessionName: string): Promise<void> {
    if (this.contextManager) {
      await this.contextManager.initialize(sessionId, sessionName);
    }
  }

  /**
   * 设置 IntentGate
   * 用于意图分类和路由决策
   */
  setIntentGate(decider: IntentDecider): void {
    this.intentGate = new IntentGate(decider);
  }

  /**
   * 获取 IntentGate
   */
  getIntentGate(): IntentGate | undefined {
    return this.intentGate;
  }

  /**
   * 分析用户意图
   * 在创建 Mission 之前调用，决定是否需要完整的 Mission 流程
   */
  async analyzeIntent(userPrompt: string): Promise<IntentGateResult | null> {
    if (!this.intentGate) {
      return null;
    }

    const result = await this.intentGate.process(userPrompt);

    this.emit('intentAnalyzed', {
      userPrompt,
      result,
    });

    return result;
  }

  /**
   * 智能处理用户请求
   * 根据意图分析结果决定处理方式
   */
  async processRequest(
    userPrompt: string,
    sessionId: string,
    options?: {
      forceMode?: IntentHandlerMode;
      projectContext?: string;
    }
  ): Promise<{
    mode: IntentHandlerMode;
    mission?: Mission;
    skipMission: boolean;
    clarificationQuestions?: string[];
    suggestion: string;
  }> {
    // 1. 意图分析（如果配置了 IntentGate）
    let mode = options?.forceMode || IntentHandlerMode.TASK;
    let suggestion = '创建任务执行';

    if (this.intentGate && !options?.forceMode) {
      const intentResult = await this.analyzeIntent(userPrompt);

      if (intentResult) {
        mode = intentResult.recommendedMode;
        suggestion = intentResult.suggestion;

        if (intentResult.needsClarification) {
          return {
            mode: IntentHandlerMode.CLARIFY,
            skipMission: true,
            clarificationQuestions: intentResult.clarificationQuestions,
            suggestion,
          };
        }

        // 对于快速模式，跳过 Mission 创建
        if (
          mode === IntentHandlerMode.ASK
          || mode === IntentHandlerMode.DIRECT
          || mode === IntentHandlerMode.EXPLORE
        ) {
          return {
            mode,
            skipMission: true,
            suggestion,
          };
        }
      }
    }

    // 2. 对于 TASK 和 EXPLORE 模式，创建 Mission
    const mission = await this.createMission({
      userPrompt,
      sessionId,
      context: options?.projectContext,
    });

    return {
      mode,
      mission,
      skipMission: false,
      suggestion,
    };
  }

  

  /**
   * 设置存储层事件监听
   */
  private setupStorageListeners(): void {
    this.storage.on('missionCreated', (data) => {
      this.emit('missionCreated', data);
    });

    this.storage.on('missionStatusChanged', (data) => {
      this.emit('missionStatusChanged', data);
    });

    this.storage.on('missionPhaseChanged', (data) => {
      this.emit('missionPhaseChanged', data);
    });
  }

  /**
   * 创建新 Mission
   */
  async createMission(params: CreateMissionParams): Promise<Mission> {
    // 增强用户提示，注入项目知识库上下文
    let enhancedContext = params.context || '';

    if (this.projectKnowledgeBase) {
      const projectContext = this.getProjectContext(600);
      const relevantADRs = this.getRelevantADRs(params.userPrompt);

      const knowledgeParts: string[] = [];
      if (projectContext) {
        knowledgeParts.push('## 项目信息');
        knowledgeParts.push(projectContext);
      }
      if (relevantADRs) {
        knowledgeParts.push('\n## 相关架构决策');
        knowledgeParts.push(relevantADRs);
      }

      if (knowledgeParts.length > 0) {
        enhancedContext = knowledgeParts.join('\n') + (enhancedContext ? '\n\n' + enhancedContext : '');
        logger.info('任务编排器.知识库.上下文已注入', {
          hasProjectContext: !!projectContext,
          hasADRs: !!relevantADRs
        }, LogCategory.ORCHESTRATOR);
      }
    }

    const mission = await this.storage.createMission({
      ...params,
      context: enhancedContext
    });
    return mission;
  }

  /**
   * 理解目标阶段
   * 将用户请求转化为结构化的目标
   */
  async understandGoal(
    mission: Mission,
    analysis: {
      goal: string;
      analysis: string;
      constraints?: string[];
      acceptanceCriteria?: string[];
      riskLevel?: 'low' | 'medium' | 'high';
      riskFactors?: string[];
    }
  ): Promise<Mission> {
    mission.goal = analysis.goal;
    mission.analysis = analysis.analysis;

    if (analysis.constraints) {
      mission.constraints = analysis.constraints.map((desc, i) => ({
        id: `constraint_${i}`,
        type: 'must' as const,
        description: desc,
        source: 'system' as const,
      }));

      // 【新增】将约束条件记录到 Memory
      if (this.contextManager) {
        for (const constraint of analysis.constraints) {
          this.contextManager.addUserConstraint(constraint);
        }
      }
    }

    if (analysis.acceptanceCriteria) {
      mission.acceptanceCriteria = analysis.acceptanceCriteria.map((desc, i) => {
        const spec = this.parseVerificationSpec(desc);
        return {
          id: `criterion_${i}`,
          description: desc,
          verifiable: true,
          verificationMethod: spec ? 'auto' : 'manual',
          status: 'pending' as const,
          verificationSpec: spec,
        };
      });
    } else {
      mission.acceptanceCriteria = [
        {
          id: 'criterion_0',
          description: '任务完成',
          verifiable: true,
          verificationMethod: 'auto',
          status: 'pending' as const,
          verificationSpec: { type: 'task_completed' },
        },
      ];
    }

    mission.riskLevel = analysis.riskLevel || 'medium';
    mission.riskFactors = analysis.riskFactors || [];
    mission.phase = 'participant_selection';

    // 【新增】设置当前工作状态
    if (this.contextManager) {
      this.contextManager.setCurrentWork(`理解目标: ${analysis.goal}`);
    }

    await this.storage.update(mission);

    this.emit('goalUnderstood', { mission });

    return mission;
  }

  /**
   * 选择参与者
   * 基于画像自动选择合适的 Worker
   */
  async selectParticipants(
    mission: Mission,
    options?: { preferredWorkers?: WorkerSlot[]; category?: string; categories?: string[] }
  ): Promise<WorkerSlot[]> {
    const allProfiles = this.profileLoader.getAllProfiles();
    const participants: WorkerSlot[] = [];
    const connectedWorkers = this.getConnectedWorkers(allProfiles);

    if (options?.preferredWorkers && options.preferredWorkers.length > 0) {
      throw new Error('禁止指定 Worker：参与者必须由分类归属唯一决定');
    }

    const categories = options?.categories && options.categories.length > 0
      ? options.categories
      : options?.category
        ? [options.category]
        : [];

    logger.info('编排器.选择参与者.分类', {
      categories,
      connectedWorkers: Array.from(connectedWorkers)
    }, LogCategory.ORCHESTRATOR);

    if (categories.length === 0) {
      throw new Error('缺少任务分类，无法选择参与者');
    }

    for (const category of categories) {
      if (!this.profileLoader.getCategory(category)) {
        throw new Error(`未知分类: ${category}`);
      }
      const worker = this.assignmentResolver.resolveWorker(category);
      if (!connectedWorkers.has(worker)) {
        throw new Error(`分类 "${category}" 归属 ${worker}，但当前不可用`);
      }
      participants.push(worker);
    }

    const uniqueParticipants = Array.from(new Set(participants));

    logger.info('编排器.选择参与者.结果', {
      uniqueParticipants,
      rawParticipants: participants
    }, LogCategory.ORCHESTRATOR);

    if (uniqueParticipants.length === 0) {
      throw new Error('未能选择到任何可用 Worker');
    }

    mission.phase = 'contract_definition';
    await this.storage.update(mission);

    this.emit('participantsSelected', { missionId: mission.id, participants: uniqueParticipants });

    return uniqueParticipants;
  }

  private getConnectedWorkers(allProfiles: Map<WorkerSlot, unknown>): Set<WorkerSlot> {
    const connected = new Set<WorkerSlot>();
    for (const worker of allProfiles.keys()) {
      if (this.isWorkerAvailable(worker)) {
        connected.add(worker);
      }
    }
    return connected;
  }

  private isWorkerAvailable(worker: WorkerSlot): boolean {
    if (this.adapterFactory.isConnected(worker)) {
      return true;
    }
    try {
      const workers = LLMConfigLoader.loadWorkersConfig();
      const cfg = workers[worker];
      return Boolean(cfg?.enabled && cfg.baseUrl && cfg.model);
    } catch {
      return false;
    }
  }

  /**
   * 定义契约
   */
  async defineContracts(
    mission: Mission,
    participants: WorkerSlot[]
  ): Promise<Contract[]> {
    const contracts = await this.contractManager.defineContracts(mission, participants);

    mission.contracts = contracts;
    mission.phase = 'responsibility_assignment';
    await this.storage.update(mission);

    this.emit('contractsDefined', { missionId: mission.id, contracts });

    return contracts;
  }

  /**
   * 分配职责
   */
  async assignResponsibilities(
    mission: Mission,
    participants: WorkerSlot[],
    options?: {
      routingCategory?: string;
      routingCategories?: Record<string, string>;
      routingReason?: string;
      requiresModification?: boolean;
      delegationBriefings?: string[];
    }
  ): Promise<Assignment[]> {
    const taskInfo = this.buildTaskStructuredInfo(mission);
    const additionalContext = this.contextManager
      ? await this.contextManager.getAssembledContextText(
          this.contextManager.buildAssemblyOptions(mission.id, 'orchestrator', 1200),
          { excludePartTypes: ['recent_turns'] }
        )
      : mission.context;
    const assignments = await this.assignmentManager.createAssignments(
      mission,
      participants,
      mission.contracts,
      {
        taskInfo,
        additionalContext,
        routingCategory: options?.routingCategory,
        routingCategories: options?.routingCategories,
        routingReason: options?.routingReason,
        requiresModification: options?.requiresModification,
        delegationBriefings: options?.delegationBriefings,
      }
    );

    mission.assignments = assignments;
    mission.phase = 'worker_planning';
    await this.storage.update(mission);

    this.emit('responsibilitiesAssigned', { missionId: mission.id, assignments });

    return assignments;
  }

  /**
   * 任务结构化信息 - 提案 4.3
   */
  private buildTaskStructuredInfo(mission: Mission): TaskStructuredInfo | undefined {
    const taskInfo: TaskStructuredInfo = {};

    if (mission.acceptanceCriteria?.length) {
      taskInfo.expectedOutcome = mission.acceptanceCriteria.map(c => c.description).filter(Boolean);
    }

    if (mission.constraints?.length) {
      const mustDo = mission.constraints
        .filter(c => c.type === 'must' || c.type === 'should')
        .map(c => c.description)
        .filter(Boolean);
      const mustNotDo = mission.constraints
        .filter(c => c.type === 'must_not' || c.type === 'should_not')
        .map(c => c.description)
        .filter(Boolean);

      if (mustDo.length > 0) {
        taskInfo.mustDo = mustDo;
      }
      if (mustNotDo.length > 0) {
        taskInfo.mustNotDo = mustNotDo;
      }
    }

    const memory = this.contextManager.getMemoryDocument()?.getContent();
    if (memory) {
      if (memory.keyDecisions?.length) {
        taskInfo.relatedDecisions = memory.keyDecisions
          .slice(-5)
          .map(d => `${d.description}: ${d.reason}`);
      }
      if (memory.pendingIssues?.length) {
        // 将 Issue 对象转换为 string 数组
        taskInfo.pendingIssues = memory.pendingIssues
          .slice(-5)
          .map(issue => issue.description);
      }
    }

    const hasContent =
      (taskInfo.expectedOutcome && taskInfo.expectedOutcome.length > 0) ||
      (taskInfo.mustDo && taskInfo.mustDo.length > 0) ||
      (taskInfo.mustNotDo && taskInfo.mustNotDo.length > 0) ||
      (taskInfo.relatedDecisions && taskInfo.relatedDecisions.length > 0) ||
      (taskInfo.pendingIssues && taskInfo.pendingIssues.length > 0);

    return hasContent ? taskInfo : undefined;
  }

  /**
   * 批准 Mission 开始执行
   */
  async approveMission(missionId: string): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.status = 'executing';
    mission.phase = 'execution';
    mission.startedAt = Date.now();
    await this.storage.update(mission);

    this.emit('missionApproved', { mission });

    return mission;
  }

  /**
   * 暂停 Mission
   */
  async pauseMission(missionId: string, reason?: string): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.status = 'paused';
    await this.storage.update(mission);

    this.emit('missionPaused', { mission, reason });

    return mission;
  }

  /**
   * 恢复 Mission
   */
  async resumeMission(missionId: string): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    if (mission.status !== 'paused') {
      throw new Error(`Mission is not paused: ${mission.status}`);
    }

    mission.status = 'executing';
    await this.storage.update(mission);

    this.emit('missionResumed', { mission });

    return mission;
  }

  /**
   * 取消 Mission
   */
  async cancelMission(missionId: string, reason?: string): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.status = 'cancelled';
    await this.storage.update(mission);

    this.emit('missionCancelled', { mission, reason });

    return mission;
  }

  /**
   * 完成 Mission
   */
  async completeMission(missionId: string, missionOverride?: Mission): Promise<Mission> {
    const mission = missionOverride ?? await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.status = 'completed';
    mission.phase = 'summary';
    mission.completedAt = Date.now();
    await this.storage.update(mission);

    // 清理该 Mission 的共享上下文
    if (this.contextManager) {
      this.contextManager.clearMissionContext(missionId);
    }

    this.emit('missionCompleted', { mission });

    return mission;
  }

  /**
   * Mission 失败
   */
  async failMission(missionId: string, error: string, missionOverride?: Mission): Promise<Mission> {
    const mission = missionOverride ?? await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.status = 'failed';
    await this.storage.update(mission);

    // 清理该 Mission 的共享上下文
    if (this.contextManager) {
      this.contextManager.clearMissionContext(missionId);
    }

    this.emit('missionFailed', { mission, error });

    return mission;
  }

  /**
   * 获取 Mission
   */
  async getMission(missionId: string): Promise<Mission | null> {
    return this.storage.load(missionId);
  }

  /**
   * 获取会话的所有 Mission
   */
  async getSessionMissions(sessionId: string): Promise<Mission[]> {
    return this.storage.listBySession(sessionId);
  }

  /**
   * 更新 Assignment
   */
  async updateAssignment(missionId: string, assignment: Assignment): Promise<void> {
    await this.storage.updateAssignment(missionId, assignment);
  }

  /**
   * 更新 Contract
   */
  async updateContract(missionId: string, contract: Contract): Promise<void> {
    await this.storage.updateContract(missionId, contract);
  }

  /**
   * 获取 ProfileLoader
   */
  getProfileLoader(): ProfileLoader {
    return this.profileLoader;
  }

  /**
   * 获取 GuidanceInjector
   */
  getGuidanceInjector(): GuidanceInjector {
    return this.guidanceInjector;
  }

  /**
   * 获取 Reviewer
   */
  getReviewer(): ProfileAwareReviewer {
    return this.reviewer;
  }

  /**
   * 验证 Mission 完成情况
   * Phase 8: 检验验收标准、契约履行、技术验证
   */
  async verifyMission(
    missionId: string,
    options?: {
      runTechnicalVerification?: boolean;
      verificationConfig?: Partial<VerificationConfig>;
    }
  ): Promise<MissionVerificationResult> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.phase = 'verification';
    await this.storage.update(mission);

    this.emit('verificationStarted', { missionId });

    const result: MissionVerificationResult = {
      passed: true,
      criteriaStatus: [],
      contractsVerified: true,
      contractViolations: [],
      summary: '',
    };

    // 1. 验证验收标准
    for (const criterion of mission.acceptanceCriteria) {
      const { passed, reason } = this.verifyCriterion(criterion, mission);

      result.criteriaStatus.push({
        criterionId: criterion.id,
        description: criterion.description,
        passed,
        reason,
      });

      // 更新 Mission 中的验收标准状态
      criterion.status = passed ? 'passed' : 'failed';
    }

    // 2. 验证契约履行
    for (const contract of mission.contracts) {
      if (contract.status !== 'verified') {
        // 检查契约是否被实现
        const producer = mission.assignments.find(a => a.workerId === contract.producer);
        if (producer) {
          const implementedTodo = producer.todos.find(t =>
            t.producesContracts.includes(contract.id) && t.status === 'completed'
          );

          if (implementedTodo) {
            contract.status = 'implemented';
            // 简单验证：假设完成即验证通过
            contract.status = 'verified';
          } else {
            result.contractsVerified = false;
            result.contractViolations.push(
              `契约 "${contract.name}" 未被实现 (生产者: ${contract.producer})`
            );
          }
        }
      }
    }

    // 3. 技术验证（编译、测试等）
    if (options?.runTechnicalVerification && this.verificationRunner) {
      if (options.verificationConfig) {
        this.verificationRunner.updateConfig(options.verificationConfig);
      }

      // 收集所有修改的文件
      const modifiedFiles: string[] = [];
      for (const assignment of mission.assignments) {
        for (const todo of assignment.todos) {
          if (todo.output?.modifiedFiles) {
            modifiedFiles.push(...todo.output.modifiedFiles);
          }
        }
      }

      result.technicalVerification = await this.verificationRunner.runVerification(
        missionId,
        [...new Set(modifiedFiles)]
      );

      if (!result.technicalVerification.success) {
        result.passed = false;
      }
    }

    // 4. 综合判断
    const criteriaFailed = result.criteriaStatus.filter(c => !c.passed);
    if (criteriaFailed.length > 0) {
      result.passed = false;
    }
    if (!result.contractsVerified) {
      result.passed = false;
    }

    // 5. 生成总结
    const summaryParts: string[] = [];
    summaryParts.push(`验收标准: ${result.criteriaStatus.filter(c => c.passed).length}/${result.criteriaStatus.length} 通过`);
    summaryParts.push(`契约验证: ${result.contractsVerified ? '通过' : '存在违反'}`);
    if (result.technicalVerification) {
      summaryParts.push(`技术验证: ${result.technicalVerification.success ? '通过' : '失败'}`);
    }
    result.summary = summaryParts.join('; ');

    // 6. 更新 Mission 状态
    if (result.passed) {
      mission.phase = 'summary';
    }
    await this.storage.update(mission);

    this.emit('verificationCompleted', { missionId, result });

    return result;
  }

  /**
   * 验证单个验收标准
   * 优先使用结构化 verificationSpec，否则回退到任务完成检查
   */
  private verifyCriterion(
    criterion: AcceptanceCriterion,
    mission: Mission
  ): { passed: boolean; reason?: string } {
    // 如果不可验证，直接通过
    if (!criterion.verifiable) {
      return { passed: true };
    }

    const spec = criterion.verificationSpec;

    // 有结构化规格时，使用结构化验证
    if (spec) {
      if (spec.type === 'task_completed') {
        return this.verifyByTaskCompletion(criterion.description, mission);
      }
      return this.verifyWithSpec(spec);
    }

    // 没有结构化规格时，回退到任务完成检查
    return this.verifyByTaskCompletion(criterion.description, mission);
  }

  /**
   * 使用结构化规格验证
   */
  private verifyWithSpec(spec: VerificationSpec): { passed: boolean; reason?: string } {
    switch (spec.type) {
      case 'file_exists': {
        if (!spec.targetPath) {
          return { passed: false, reason: '验证规格缺少 targetPath' };
        }
        const resolvedPath = this.resolvePath(spec.targetPath);
        const exists = fs.existsSync(resolvedPath);
        return exists
          ? { passed: true }
          : { passed: false, reason: `文件不存在: ${resolvedPath}` };
      }

      case 'file_content': {
        if (!spec.targetPath) {
          return { passed: false, reason: '验证规格缺少 targetPath' };
        }
        if (spec.expectedContent === undefined) {
          return { passed: false, reason: '验证规格缺少 expectedContent' };
        }
        const resolvedPath = this.resolvePath(spec.targetPath);
        if (!fs.existsSync(resolvedPath)) {
          return { passed: false, reason: `文件不存在: ${resolvedPath}` };
        }
        const actual = fs.readFileSync(resolvedPath, 'utf8');
        const matchMode = spec.contentMatchMode || 'exact';
        let matched = false;
        switch (matchMode) {
          case 'exact':
            matched = actual === spec.expectedContent;
            break;
          case 'contains':
            matched = actual.includes(spec.expectedContent);
            break;
          case 'regex':
            try {
              matched = new RegExp(spec.expectedContent).test(actual);
            } catch {
              return { passed: false, reason: `无效的正则表达式: ${spec.expectedContent}` };
            }
            break;
        }
        return matched
          ? { passed: true }
          : { passed: false, reason: `文件内容不匹配: ${resolvedPath}` };
      }

      case 'task_completed': {
        // 由于没有 mission 上下文，task_completed 需要外部处理
        // 这里返回待定状态
        return { passed: false, reason: '任务完成验证需要 mission 上下文' };
      }

      case 'test_pass': {
        // 测试验证需要执行命令，暂不支持自动化
        return { passed: false, reason: '测试验证需要手动执行' };
      }

      case 'custom': {
        // 自定义验证需要外部实现
        return { passed: false, reason: '自定义验证需要外部实现' };
      }

      default:
        return { passed: false, reason: `未知的验证类型: ${(spec as VerificationSpec).type}` };
    }
  }

  private parseVerificationSpec(description: string): VerificationSpec | undefined {
    const text = (description || '').trim();
    if (!text) return undefined;

    const strip = (value: string) => value.replace(/^["'`]|["'`]$/g, '');

    // 模式1: 文件存在
    const fileExistsMatch = text.match(
      /(?:文件|file)\s+([^\s]+)\s*(?:存在|已创建|exists)/i
    ) || text.match(/创建文件\s*([^\s]+)/i);
    if (fileExistsMatch) {
      const targetPath = strip(fileExistsMatch[1]);
      return {
        type: 'file_exists',
        targetPath,
      };
    }

    // 模式2: 文件内容为/等于
    const contentWithPathMatch = text.match(
      /(?:文件|file)\s+([^\s]+)\s*(?:内容|content)\s*(?:等于|为|匹配|is|equals)\s*["'`]?([^"'`]+)["'`]?/i
    );
    if (contentWithPathMatch) {
      return {
        type: 'file_content',
        targetPath: strip(contentWithPathMatch[1]),
        expectedContent: strip(contentWithPathMatch[2].trim()),
        contentMatchMode: /匹配|match/i.test(text) ? 'contains' : 'exact',
      };
    }

    // 模式3: 写入 内容 到 路径
    const writeToMatch = text.match(/写入\s*["'`]?([^"'`]+)["'`]?\s*(?:到|至)\s*([^\s]+)/i);
    if (writeToMatch) {
      return {
        type: 'file_content',
        targetPath: strip(writeToMatch[2]),
        expectedContent: strip(writeToMatch[1].trim()),
        contentMatchMode: 'exact',
      };
    }
    const writeAtMatch = text.match(/在\s*([^\s]+)\s*(?:写入|写|write)\s*["'`]?([^"'`]+)["'`]?/i);
    if (writeAtMatch) {
      return {
        type: 'file_content',
        targetPath: strip(writeAtMatch[1]),
        expectedContent: strip(writeAtMatch[2].trim()),
        contentMatchMode: 'exact',
      };
    }

    // 模式4: 测试通过
    if (/(?:测试通过|tests?\s+pass)/i.test(text)) {
      return { type: 'test_pass' };
    }

    // 模式5: 任务完成
    if (/(?:任务完成|task\s+complet)/i.test(text)) {
      return { type: 'task_completed' };
    }

    return undefined;
  }

  /**
   * 通过任务完成状态验证
   */
  private verifyByTaskCompletion(
    description: string,
    mission: Mission
  ): { passed: boolean; reason?: string } {
    // 检查相关的 Assignment 和 Todo 是否完成
    const relatedAssignments = mission.assignments.filter(a =>
      a.todos.some(t =>
        t.content.toLowerCase().includes(description.toLowerCase().slice(0, 20))
      )
    );

    if (relatedAssignments.length === 0) {
      const allAssignmentsCompleted = mission.assignments.length > 0
        && mission.assignments.every(a => a.status === 'completed');
      return allAssignmentsCompleted
        ? { passed: true }
        : { passed: false, reason: '任务未全部完成' };
    }

    const allCompleted = relatedAssignments.every(a =>
      a.todos.every(t => t.status === 'completed' || t.status === 'skipped')
    );

    const passed = allCompleted;
    const reason = passed ? undefined : '相关任务未全部完成';

    return { passed, reason };
  }

  /**
   * 解析路径（相对路径转绝对路径）
   */
  private resolvePath(targetPath: string): string {
    if (path.isAbsolute(targetPath)) {
      return targetPath;
    }
    return this.workspaceRoot
      ? path.join(this.workspaceRoot, targetPath)
      : targetPath;
  }

  /**
   * 将任务执行结果写入 Memory
   * 记录：任务状态、关键决策、代码变更、失败原因
   */
  private async writeExecutionToMemory(mission: Mission): Promise<void> {
    const memory = this.contextManager.getMemoryDocument();
    if (!memory) {
      return;
    }

    for (const assignment of mission.assignments) {
      // 1. 添加/更新任务状态
      const taskExists = memory.getContent().currentTasks.some(t => t.id === assignment.id);

      if (!taskExists && assignment.status !== 'completed' && assignment.status !== 'failed') {
        // 添加进行中的任务
        memory.addCurrentTask({
          id: assignment.id,
          description: assignment.responsibility,
          status: assignment.status === 'executing' ? 'in_progress' : 'pending',
          assignedWorker: assignment.workerId,
        });
      }

      // 2. 更新已完成或失败的任务
      if (assignment.status === 'completed') {
        const completedTodos = assignment.todos.filter(t => t.status === 'completed');
        const summary = completedTodos.length > 0
          ? `完成 ${completedTodos.length} 个子任务`
          : '任务完成';
        memory.updateTaskStatus(assignment.id, 'completed', summary);
      } else if (assignment.status === 'failed') {
        const failedTodos = assignment.todos.filter(t => t.status === 'failed');
        const errors = failedTodos
          .map(t => t.output?.error)
          .filter(Boolean)
          .join('; ');
        memory.updateTaskStatus(assignment.id, 'failed', errors || '执行失败');

        // 3. 记录失败原因到 pendingIssues
        if (errors) {
          memory.addPendingIssue(`[${assignment.workerId}] ${assignment.responsibility}: ${errors}`);
        }
      }

      // 4. 记录代码变更
      for (const todo of assignment.todos) {
        if (todo.status === 'completed' && todo.output?.modifiedFiles) {
          for (const file of todo.output.modifiedFiles) {
            memory.addCodeChange({
              file,
              summary: `${todo.type || 'task'} (${assignment.workerId})`,
              action: 'modify',
            });
          }
        }
      }

      // 5. 记录关键决策（如果有）
      if (assignment.status === 'completed' && assignment.responsibility) {
        const hasDecisionKeywords = /决策|选择|方案|架构|设计/.test(assignment.responsibility);
        if (hasDecisionKeywords) {
          memory.addDecision({
            id: `decision-${assignment.id}`,
            description: `${assignment.workerId}: ${assignment.responsibility}`,
            reason: `完成 ${assignment.todos.filter(t => t.status === 'completed').length} 个子任务`,
          });
        }
      }

      // 【新增】6. 任务成功完成时，将之前的 pendingIssue 标记为已解决
      if (assignment.status === 'completed') {
        const pendingIssues = memory.getContent().pendingIssues;
        const relatedIssue = pendingIssues.find(issue => issue.description.includes(assignment.workerId));
        if (relatedIssue) {
          memory.markIssueResolved(
            relatedIssue.id,
            '任务重试后成功完成',
            `${assignment.workerId} 完成了 ${assignment.todos.filter(t => t.status === 'completed').length} 个子任务`
          );
        }
      }
    }

    // 7. 保存 Memory（含压缩）
    if (memory.isDirty()) {
      await this.contextManager.saveMemory();
      logger.info('编排器.Memory.写回完成', { missionId: mission.id }, LogCategory.ORCHESTRATOR);
    }
  }

  /**
   * 生成 Mission 总结
   * Phase 9: 汇总执行情况，生成报告
   */
  async summarizeMission(missionId: string): Promise<MissionSummary> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    // 在生成总结前，先将执行结果写入 Memory
    await this.writeExecutionToMemory(mission);

    this.emit('summarizationStarted', { missionId });

    // 收集统计数据
    const modifiedFiles: string[] = [];
    let completedTodos = 0;
    let failedTodos = 0;
    let skippedTodos = 0;
    let recoveryAttempts = 0;

    const workerContributions: MissionSummary['workerContributions'] = {} as MissionSummary['workerContributions'];

    for (const assignment of mission.assignments) {
      const workerId = assignment.workerId;

      if (!workerContributions[workerId]) {
        workerContributions[workerId] = {
          assignmentCount: 0,
          completedTodos: 0,
          failedTodos: 0,
        };
      }

      workerContributions[workerId].assignmentCount++;

      for (const todo of assignment.todos) {
        if (todo.status === 'completed') {
          completedTodos++;
          workerContributions[workerId].completedTodos++;
          if (todo.output?.modifiedFiles) {
            modifiedFiles.push(...todo.output.modifiedFiles);
          }
        } else if (todo.status === 'failed') {
          failedTodos++;
          workerContributions[workerId].failedTodos++;
        } else if (todo.status === 'skipped') {
          skippedTodos++;
        }

        // 统计恢复尝试（从 Todo 历史或重试计数）
        if (todo.retryCount) {
          recoveryAttempts += todo.retryCount;
        }
      }
    }

    // 识别关键成就
    const keyAchievements: string[] = [];
    for (const criterion of mission.acceptanceCriteria) {
      if (criterion.status === 'passed') {
        keyAchievements.push(criterion.description);
      }
    }

    // 识别遗留问题
    const remainingIssues: string[] = [];
    for (const criterion of mission.acceptanceCriteria) {
      if (criterion.status === 'failed' || criterion.status === 'pending') {
        remainingIssues.push(`未完成: ${criterion.description}`);
      }
    }
    for (const assignment of mission.assignments) {
      for (const todo of assignment.todos) {
        if (todo.status === 'failed') {
          remainingIssues.push(`失败: ${todo.content}`);
        }
      }
    }

    // 建议后续步骤
    const suggestedNextSteps: string[] = [];
    if (failedTodos > 0) {
      suggestedNextSteps.push('修复失败的任务');
    }
    if (remainingIssues.length > 0) {
      suggestedNextSteps.push('处理遗留问题');
    }
    if (mission.status === 'completed' && failedTodos === 0) {
      suggestedNextSteps.push('进行代码审查');
      suggestedNextSteps.push('更新文档');
    }

    const summary: MissionSummary = {
      missionId: mission.id,
      goal: mission.goal || '',
      success: mission.status === 'completed' && failedTodos === 0,
      duration: mission.completedAt && mission.startedAt
        ? mission.completedAt - mission.startedAt
        : 0,
      modifiedFiles: [...new Set(modifiedFiles)],
      completedTodos,
      failedTodos,
      skippedTodos,
      recoveryAttempts,
      workerContributions,
      keyAchievements,
      remainingIssues,
      suggestedNextSteps,
    };

    this.emit('summarizationCompleted', { missionId, summary });

    return summary;
  }

  // ============================================================================
  // 缓存管理
  // ============================================================================

  /**
   * 生成缓存键
   */
  private generateCacheKey(prompt: string, sessionId: string): string {
    // 简单的 hash 实现
    let hash = 0;
    const str = `${sessionId}:${prompt}`;
    for (let i = 0; i < str.length; i++) {
      const char = str.charCodeAt(i);
      hash = ((hash << 5) - hash) + char;
      hash = hash & hash;
    }
    return `cache_${hash}`;
  }

  /**
   * 从缓存获取规划结果
   */
  getCachedPlanning(prompt: string, sessionId: string): Mission | null {
    const key = this.generateCacheKey(prompt, sessionId);
    const cached = this.planningCache.get(key);

    if (!cached) return null;

    // 检查是否过期
    if (Date.now() - cached.timestamp > this.CACHE_TTL_MS) {
      this.planningCache.delete(key);
      return null;
    }

    return cached.mission;
  }

  /**
   * 缓存规划结果
   */
  cachePlanning(prompt: string, sessionId: string, mission: Mission): void {
    const key = this.generateCacheKey(prompt, sessionId);
    this.planningCache.set(key, {
      mission,
      timestamp: Date.now(),
    });

    // 清理过期缓存（限制缓存大小）
    if (this.planningCache.size > 100) {
      this.cleanupCache();
    }
  }

  /**
   * 清理过期缓存
   */
  private cleanupCache(): void {
    const now = Date.now();
    for (const [key, value] of this.planningCache) {
      if (now - value.timestamp > this.CACHE_TTL_MS) {
        this.planningCache.delete(key);
      }
    }
  }

  /**
   * 清空所有缓存
   */
  clearCache(): void {
    this.planningCache.clear();
  }

  // ============================================================================
  // 执行相关方法（从 MissionExecutor 合并）
  // ============================================================================

  /**
   * 确保 Worker 存在（懒加载创建）
   */
  private async ensureWorker(workerSlot: WorkerSlot): Promise<AutonomousWorker> {
    let worker = this.workers.get(workerSlot);
    if (!worker) {
      // 确保 TodoManager 存在
      if (!this.todoManager && this.workspaceRoot) {
        try {
          this.todoManager = new TodoManager(this.workspaceRoot);
          await this.todoManager.initialize();
          logger.info('编排器.TodoManager.已初始化', {
            workspaceRoot: this.workspaceRoot,
            forWorker: workerSlot,
          }, LogCategory.ORCHESTRATOR);
        } catch (error) {
          const errorMessage = error instanceof Error ? error.message : String(error);
          logger.error('编排器.TodoManager.初始化失败', {
            error: errorMessage,
            workspaceRoot: this.workspaceRoot,
            workerSlot,
          }, LogCategory.ORCHESTRATOR);
          throw new Error(`初始化 TodoManager 失败 (workspace: ${this.workspaceRoot}, worker: ${workerSlot}): ${errorMessage}`);
        }
      }
      if (!this.todoManager) {
        throw new Error('未配置 TodoManager，无法创建 Worker');
      }
      // 确保 ContextManager 存在以获取共享上下文依赖
      const sharedContextDeps = {
        contextAssembler: this.contextManager.getContextAssembler(),
        fileSummaryCache: this.contextManager.getFileSummaryCache(),
        sharedContextPool: this.contextManager.getSharedContextPool(),
      };
      worker = new AutonomousWorker(
        workerSlot,
        this.profileLoader,
        this.guidanceInjector,
        this.todoManager,
        sharedContextDeps
      );
      this.workers.set(workerSlot, worker);

      worker.on('sessionCreated', (data: { sessionId: string; assignmentId: string }) => {
        this.emit('workerSessionCreated', {
          ...data,
          workerId: workerSlot,
        });
      });

      worker.on('sessionResumed', (data: { sessionId: string; assignmentId: string; completedTodos: number }) => {
        this.emit('workerSessionResumed', {
          ...data,
          workerId: workerSlot,
        });
      });

      // 🔧 转发 Todo 事件，确保 UI 能实时更新子任务状态
      worker.on('todoStarted', (data) => this.emit('todoStarted', { ...data, missionId: this.currentMissionId }));
      worker.on('todoCompleted', (data) => this.emit('todoCompleted', { ...data, missionId: this.currentMissionId }));
      worker.on('todoFailed', (data) => this.emit('todoFailed', { ...data, missionId: this.currentMissionId }));
      worker.on('dynamicTodoAdded', (data) => this.emit('dynamicTodoAdded', { ...data, missionId: this.currentMissionId }));
      worker.on('insightGenerated', (data) => this.emit('insightGenerated', { ...data, missionId: this.currentMissionId }));

      logger.info('编排器.Worker.创建', { workerSlot }, LogCategory.ORCHESTRATOR);
    }
    return worker;
  }

  /**
   * 批准 Todo (用于动态任务审批)
   */
  async approveTodo(todoId: string): Promise<void> {
    if (!this.todoManager) {
      throw new Error('TodoManager not initialized');
    }
    await this.todoManager.approve(todoId);
  }

  /**
   * 获取 Worker
   */
  getWorker(workerType: WorkerSlot): AutonomousWorker | undefined {
    return this.workers.get(workerType);
  }

  /**
   * 确保 Worker 存在（公开接口，供 dispatch_task 使用）
   */
  async ensureWorkerForDispatch(workerSlot: WorkerSlot): Promise<AutonomousWorker> {
    return this.ensureWorker(workerSlot);
  }

  /**
   * 获取 TodoManager 实例（确保已初始化）
   */
  getTodoManager(): TodoManager | undefined {
    return this.todoManager;
  }

  /**
   * 获取所有 Worker
   */
  getAllWorkers(): Map<WorkerSlot, AutonomousWorker> {
    return new Map(this.workers);
  }

  /**
   * 获取当前执行中的 Mission ID
   */
  getCurrentMissionId(): string | null {
    return this.currentMissionId;
  }

  /**
   * 设置当前 Mission ID（供 DispatchManager 编排模式使用）
   * 确保 Worker 转发的 Todo 事件能关联到正确的 Mission
   */
  setCurrentMissionId(missionId: string | null): void {
    this.currentMissionId = missionId;
  }

  /**
   * 检查是否正在执行 Mission
   */
  isExecuting(): boolean {
    return this.currentMissionId !== null;
  }

  /**
   * 销毁编排器（清理资源）
   */
  dispose(): void {
    // 销毁所有 Worker
    for (const worker of this.workers.values()) {
      worker.dispose();
    }
    this.workers.clear();

    // 清理缓存
    this.planningCache.clear();

    // 移除所有事件监听器
    this.removeAllListeners();

    logger.info('任务编排器.销毁', undefined, LogCategory.ORCHESTRATOR);
  }
}
