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
import { CLIType } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
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
import { UnifiedSessionManager } from '../../session/unified-session-manager';
import {
  MissionStorageManager,
  ContractManager,
  AssignmentManager,
  Mission,
  Contract,
  Assignment,
  CreateMissionParams,
} from '../mission';

/**
 * Mission 创建结果
 */
export interface MissionCreationResult {
  mission: Mission;
  contracts: Contract[];
  assignments: Assignment[];
}

/**
 * 规划选项
 */
export interface PlanningOptions {
  /** 参与者列表（如果不指定，自动选择） */
  participants?: CLIType[];
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
  workerContributions: Record<CLIType, {
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
  private contextManager?: ContextManager;

  // 规划结果缓存（基于 prompt hash）
  private planningCache: Map<string, { mission: Mission; timestamp: number }> = new Map();
  private readonly CACHE_TTL_MS = 5 * 60 * 1000; // 5 分钟缓存过期

  constructor(
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector,
    storage?: MissionStorageManager,
    private workspaceRoot?: string
  ) {
    super();
    this.storage = storage || new MissionStorageManager();
    this.contractManager = new ContractManager();
    this.assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    this.reviewer = new ProfileAwareReviewer(profileLoader);

    if (workspaceRoot) {
      this.verificationRunner = new VerificationRunner(workspaceRoot);
      this.contextManager = new ContextManager(workspaceRoot);
    }

    this.setupStorageListeners();
  }

  /**
   * 设置 SnapshotManager
   * 用于文件快照和回滚
   */
  setSnapshotManager(snapshotManager: SnapshotManager): void {
    this.snapshotManager = snapshotManager;
  }

  /**
   * 获取 SnapshotManager
   */
  getSnapshotManager(): SnapshotManager | undefined {
    return this.snapshotManager;
  }

  /**
   * 设置 ContextManager
   * 用于上下文管理
   */
  setContextManager(contextManager: ContextManager): void {
    this.contextManager = contextManager;
  }

  /**
   * 获取 ContextManager
   */
  getContextManager(): ContextManager | undefined {
    return this.contextManager;
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

        // 对于简单模式，跳过 Mission 创建
        if (mode === IntentHandlerMode.ASK || mode === IntentHandlerMode.DIRECT) {
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
    const mission = await this.storage.createMission(params);
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
    }

    if (analysis.acceptanceCriteria) {
      mission.acceptanceCriteria = analysis.acceptanceCriteria.map((desc, i) => ({
        id: `criterion_${i}`,
        description: desc,
        verifiable: true,
        status: 'pending' as const,
      }));
    }

    mission.riskLevel = analysis.riskLevel || 'medium';
    mission.riskFactors = analysis.riskFactors || [];
    mission.phase = 'participant_selection';

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
    options?: { preferredWorkers?: CLIType[] }
  ): Promise<CLIType[]> {
    const allProfiles = this.profileLoader.getAllProfiles();
    const participants: CLIType[] = [];

    // 如果指定了首选 Worker
    if (options?.preferredWorkers && options.preferredWorkers.length > 0) {
      participants.push(...options.preferredWorkers);
    } else {
      // 基于任务分析自动选择
      const goalText = `${mission.goal} ${mission.analysis}`.toLowerCase();

      for (const [cli, profile] of allProfiles.entries()) {
        // 检查是否有匹配的分类偏好
        const hasMatch = profile.preferences.preferredCategories.some(cat =>
          goalText.includes(cat)
        );

        // 检查是否有匹配的优势
        const hasStrength = profile.profile.strengths.some(s =>
          goalText.includes(s.toLowerCase())
        );

        if (hasMatch || hasStrength) {
          participants.push(cli as CLIType);
        }
      }

      // 如果没有匹配，默认选择第一个
      if (participants.length === 0) {
        const firstCli = allProfiles.keys().next().value;
        if (firstCli) {
          participants.push(firstCli as CLIType);
        }
      }
    }

    mission.phase = 'contract_definition';
    await this.storage.update(mission);

    this.emit('participantsSelected', { missionId: mission.id, participants });

    return participants;
  }

  /**
   * 定义契约
   */
  async defineContracts(
    mission: Mission,
    participants: CLIType[]
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
    participants: CLIType[]
  ): Promise<Assignment[]> {
    const assignments = await this.assignmentManager.createAssignments(
      mission,
      participants,
      mission.contracts
    );

    mission.assignments = assignments;
    mission.phase = 'worker_planning';
    await this.storage.update(mission);

    this.emit('responsibilitiesAssigned', { missionId: mission.id, assignments });

    return assignments;
  }

  /**
   * 完整规划流程
   * 一次性完成从目标理解到职责分配
   */
  async planMission(
    mission: Mission,
    goalAnalysis: {
      goal: string;
      analysis: string;
      constraints?: string[];
      acceptanceCriteria?: string[];
      riskLevel?: 'low' | 'medium' | 'high';
      riskFactors?: string[];
    },
    options?: PlanningOptions
  ): Promise<MissionCreationResult> {
    // 1. 理解目标
    mission = await this.understandGoal(mission, goalAnalysis);

    // 2. 选择参与者
    const participants = options?.participants ||
      await this.selectParticipants(mission);

    // 3. 定义契约
    const contracts = await this.defineContracts(mission, participants);

    // 4. 分配职责
    const assignments = await this.assignResponsibilities(mission, participants);

    // 5. 评审规划
    const reviewResult = await this.reviewer.reviewPlan(mission);

    if (reviewResult.issues.length > 0) {
      this.emit('planReviewIssues', {
        missionId: mission.id,
        issues: reviewResult.issues,
        suggestions: reviewResult.suggestions,
      });
    }

    // 6. 更新状态
    if (options?.requireApproval) {
      mission.status = 'pending_approval';
    } else {
      mission.status = 'planning';
    }
    await this.storage.update(mission);

    this.emit('missionPlanned', { mission, contracts, assignments });

    return { mission, contracts, assignments };
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
  async completeMission(missionId: string): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.status = 'completed';
    mission.phase = 'summary';
    mission.completedAt = Date.now();
    await this.storage.update(mission);

    this.emit('missionCompleted', { mission });

    return mission;
  }

  /**
   * Mission 失败
   */
  async failMission(missionId: string, error: string): Promise<Mission> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    mission.status = 'failed';
    await this.storage.update(mission);

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
      // 检查相关的 Assignment 和 Todo 是否完成
      const relatedAssignments = mission.assignments.filter(a =>
        a.todos.some(t =>
          t.content.toLowerCase().includes(criterion.description.toLowerCase().slice(0, 20))
        )
      );

      const allCompleted = relatedAssignments.every(a =>
        a.todos.every(t => t.status === 'completed' || t.status === 'skipped')
      );

      const passed = allCompleted && relatedAssignments.length > 0;

      result.criteriaStatus.push({
        criterionId: criterion.id,
        description: criterion.description,
        passed,
        reason: passed
          ? undefined
          : relatedAssignments.length === 0
            ? '未找到相关任务'
            : '相关任务未全部完成',
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
   * 生成 Mission 总结
   * Phase 9: 汇总执行情况，生成报告
   */
  async summarizeMission(missionId: string): Promise<MissionSummary> {
    const mission = await this.storage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

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

  /**
   * 设置 VerificationRunner
   */
  setVerificationRunner(runner: VerificationRunner): void {
    this.verificationRunner = runner;
  }

  /**
   * 获取 VerificationRunner
   */
  getVerificationRunner(): VerificationRunner | undefined {
    return this.verificationRunner;
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
}
