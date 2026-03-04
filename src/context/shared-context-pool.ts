/**
 * 共享上下文池 (SharedContextPool)
 *
 * 实现跨 Worker 的知识共享机制，存储任务级共享的摘要、决策、契约、风险等上下文条目。
 *
 * 设计规范参考：
 * @see docs/context/unified-memory-plan.md 5.1 节（SharedContextEntry）
 * @see docs/context/unified-memory-plan.md 8.2 节（共享上下文去重）
 *
 * 核心功能：
 * - 添加条目（自动去重，内容相似度 > 90% 时合并来源）
 * - 按 Mission 获取条目（支持重要性、标签、来源筛选）
 * - 按类型获取条目（支持 Token 预算限制）
 * - 任务隔离（不同 Mission 的上下文互不污染）
 * - 过期检查（支持条目失效时间）
 */

// 导入 ContextSource 类型，避免重复定义
import { ContextSource } from './file-summary-cache';
import { logger, LogCategory } from '../logging';
import { estimateMixedLanguageTokenCount } from '../utils/token-estimator';

// 重新导出 ContextSource，方便使用者从此模块导入
export { ContextSource };

// ============================================================================
// 类型定义
// ============================================================================

/**
 * 共享上下文条目类型
 *
 * - decision: 编排者决策
 * - contract: 任务契约
 * - file_summary: 文件摘要
 * - risk: 风险标记
 * - constraint: 用户约束
 * - insight: Worker 洞察
 */
export type SharedContextEntryType =
  | 'decision'
  | 'contract'
  | 'file_summary'
  | 'risk'
  | 'constraint'
  | 'insight';

/**
 * 重要性级别
 *
 * 评分规则：critical(4) > high(3) > medium(2) > low(1)
 */
export type ImportanceLevel = 'critical' | 'high' | 'medium' | 'low';

/**
 * 文件引用
 *
 * 用于关联条目与具体文件，支持 hash 校验以实现自动失效
 */
export interface FileReference {
  /** 文件路径 */
  path: string;
  /** 文件内容 hash（用于变更检测） */
  hash: string;
}

/**
 * 共享上下文条目
 *
 * 存储跨 Worker 共享的摘要、决策、洞察等信息
 */
export interface SharedContextEntry {
  /** 唯一标识 */
  id: string;

  /** 任务/编排上下文范围（Mission ID），用于任务隔离 */
  missionId: string;

  /** 来源：orchestrator | claude | codex | gemini */
  source: ContextSource;

  /** 类型：decision | contract | file_summary | risk | constraint | insight */
  type: SharedContextEntryType;

  /** 精炼后的摘要文本（非原文，控制 token 消耗） */
  content: string;

  /** 标签（用于订阅筛选，如 ['architecture', 'api-design']） */
  tags: string[];

  /** 关联文件路径与 hash（可选，用于 file_summary 类型） */
  fileRefs?: FileReference[];

  /** 重要性级别 */
  importance: ImportanceLevel;

  /** 生成时间（Unix 时间戳） */
  createdAt: number;

  /** 失效时间（可选，Unix 时间戳） */
  expiresAt?: number;

  /** 多来源合并记录（当多个 Worker 产生相同结论时） */
  sources?: ContextSource[];
}

/**
 * 添加结果
 */
export interface AddResult {
  /** 执行的动作：added（新增）| merged（合并到已有条目） */
  action: 'added' | 'merged';
  /** 新增条目的 ID（action 为 added 时） */
  id?: string;
  /** 已存在条目的 ID（action 为 merged 时） */
  existingId?: string;
}

/**
 * 查询选项
 */
export interface QueryOptions {
  /** 最小重要性级别筛选 */
  minImportance?: ImportanceLevel;
  /** 订阅的标签（返回包含任一标签的条目） */
  subscribedTags?: string[];
  /** 排除的来源 */
  excludeSources?: ContextSource[];
  /** 最大 Token 预算（按估算裁剪结果） */
  maxTokens?: number;
}

/**
 * 写入验证结果
 */
export interface ValidationResult {
  /** 是否允许写入 */
  allowed: boolean;
  /** 拒绝原因（allowed 为 false 时） */
  reason?: string;
}

// ============================================================================
// 常量配置
// ============================================================================

/** 内容最大长度（约 500 tokens） */
const MAX_CONTENT_LENGTH = 2000;

/** 相似度阈值（超过此值视为重复） */
const SIMILARITY_THRESHOLD = 0.9;

/** 去重索引：前缀采样长度 */
const DEDUP_PREFIX_SAMPLE_SIZE = 96;
/** 去重索引：token 采样数量 */
const DEDUP_TOKEN_SAMPLE_COUNT = 12;
/** 去重索引：内容长度分桶大小 */
const DEDUP_LENGTH_BUCKET_SIZE = 160;

/** 重要性评分映射 */
const IMPORTANCE_SCORES: Record<ImportanceLevel, number> = {
  critical: 4,
  high: 3,
  medium: 2,
  low: 1
};

const VALID_SOURCES: ContextSource[] = ['orchestrator', 'claude', 'codex', 'gemini'];
const VALID_IMPORTANCE: ImportanceLevel[] = ['critical', 'high', 'medium', 'low'];
const VALID_TYPES: SharedContextEntryType[] = [
  'decision',
  'contract',
  'file_summary',
  'risk',
  'constraint',
  'insight',
];

// ============================================================================
// SharedContextPool 实现
// ============================================================================

/**
 * 共享上下文池
 *
 * 提供跨 Worker 的知识共享能力，支持：
 * - 自动去重（基于内容相似度）
 * - 任务隔离（按 Mission ID 分区）
 * - 重要性排序
 * - 过期清理
 */
export class SharedContextPool {
  /** 条目存储（id -> entry） */
  private entries: Map<string, SharedContextEntry> = new Map();
  /** 一级索引：missionId + type -> entryId 集合 */
  private missionTypeIndex: Map<string, Set<string>> = new Map();
  /** 二级索引：missionId + type + 内容签名 -> entryId 集合 */
  private dedupSignatureIndex: Map<string, Set<string>> = new Map();

  // --------------------------------------------------------------------------
  // 公共方法
  // --------------------------------------------------------------------------

  /**
   * 添加条目（自动去重）
   *
   * 如果存在内容相似度 > 90% 的同类型条目，则合并来源而非新增。
   *
   * @param entry - 待添加的条目
   * @returns 添加结果
   */
  add(entry: SharedContextEntry): AddResult {
    // 1. 验证条目合法性
    const validation = this.validate(entry);
    if (!validation.allowed) {
      logger.warn('共享上下文.拒绝写入', { reason: validation.reason }, LogCategory.SESSION);
      // 返回 merged 表示未实际添加，但不抛出错误
      return { action: 'merged', existingId: undefined };
    }

    // 2. 检查是否有内容相似的条目
    const duplicate = this.findDuplicate(entry);

    if (duplicate) {
      // 合并来源，不新增条目
      if (!duplicate.sources) {
        duplicate.sources = [duplicate.source];
      }
      if (!duplicate.sources.includes(entry.source)) {
        duplicate.sources.push(entry.source);
      }
      // 更新时间戳为较新的值
      duplicate.createdAt = Math.max(duplicate.createdAt, entry.createdAt);

      return { action: 'merged', existingId: duplicate.id };
    }

    // 3. 新增条目
    this.entries.set(entry.id, entry);
    this.indexEntry(entry);
    return { action: 'added', id: entry.id };
  }

  /**
   * 按 Mission 获取条目
   *
   * 支持按重要性、标签、来源筛选，并按重要性排序返回。
   *
   * @param missionId - 任务 ID
   * @param options - 查询选项
   * @returns 符合条件的条目列表（按重要性降序）
   */
  getByMission(missionId: string, options: QueryOptions = {}): SharedContextEntry[] {
    const results: SharedContextEntry[] = [];

    for (const entry of this.entries.values()) {
      // 任务隔离：只返回当前 Mission 的条目
      if (entry.missionId !== missionId) continue;

      // 过期检查
      if (entry.expiresAt && entry.expiresAt < Date.now()) continue;

      // 重要性筛选
      if (options.minImportance && !this.meetsImportance(entry, options.minImportance)) continue;

      // 标签订阅筛选（包含任一订阅标签即可）
      if (options.subscribedTags && options.subscribedTags.length > 0) {
        if (!this.matchesTags(entry, options.subscribedTags)) continue;
      }

      // 来源排除
      if (options.excludeSources && options.excludeSources.includes(entry.source)) continue;

      results.push(entry);
    }

    // 按重要性降序排序
    const sorted = results.sort((a, b) => this.importanceScore(b) - this.importanceScore(a));

    // Token 预算裁剪
    if (options.maxTokens && options.maxTokens > 0) {
      return this.trimByTokenBudget(sorted, options.maxTokens);
    }

    return sorted;
  }

  /**
   * 按类型获取条目
   *
   * @param missionId - 任务 ID
   * @param type - 条目类型
   * @param maxTokens - 最大 Token 预算（可选）
   * @returns 符合条件的条目列表
   */
  getByType(
    missionId: string,
    type: SharedContextEntryType,
    maxTokens?: number
  ): SharedContextEntry[] {
    const results: SharedContextEntry[] = [];
    const missionTypeKey = this.buildMissionTypeKey(missionId, type);
    const bucket = this.missionTypeIndex.get(missionTypeKey);
    if (!bucket || bucket.size === 0) {
      return [];
    }

    const now = Date.now();
    for (const id of bucket) {
      const entry = this.entries.get(id);
      if (!entry) continue;

      // 过期检查
      if (entry.expiresAt && entry.expiresAt < now) continue;

      results.push(entry);
    }

    // 按重要性降序排序
    const sorted = results.sort((a, b) => this.importanceScore(b) - this.importanceScore(a));

    // Token 预算裁剪
    if (maxTokens && maxTokens > 0) {
      return this.trimByTokenBudget(sorted, maxTokens);
    }

    return sorted;
  }

  /**
   * 查找重复条目（基于内容相似度）
   *
   * 同一 Mission + 同一类型 + 内容相似度 > 90% 视为重复
   *
   * @param entry - 待检查的条目
   * @returns 重复的条目（如果存在）
   */
  findDuplicate(entry: SharedContextEntry): SharedContextEntry | null {
    const now = Date.now();
    const candidateIds = this.collectDuplicateCandidateIds(entry);
    if (candidateIds.size === 0) {
      return null;
    }
    const targetLength = entry.content.length;
    const maxLengthDelta = Math.max(40, Math.floor(targetLength * 0.25));

    for (const candidateId of candidateIds) {
      const existing = this.entries.get(candidateId);
      if (!existing) continue;

      // 同一条目不参与比较
      if (existing.id === entry.id) continue;

      // mission/type 双重隔离（防御性检查）
      if (existing.missionId !== entry.missionId || existing.type !== entry.type) continue;

      // 过期条目不参与去重
      if (existing.expiresAt && existing.expiresAt < now) continue;

      // 长度差过大时无需进入高成本相似度计算
      if (Math.abs(existing.content.length - targetLength) > maxLengthDelta) continue;

      if (this.similarity(existing.content, entry.content) > SIMILARITY_THRESHOLD) {
        return existing;
      }
    }

    return null;
  }

  /**
   * 计算内容相似度
   *
   * 使用编辑距离算法计算两个字符串的相似度。
   * 返回值范围 [0, 1]，1 表示完全相同。
   *
   * @param a - 字符串 A
   * @param b - 字符串 B
   * @returns 相似度（0-1）
   */
  similarity(a: string, b: string): number {
    if (a === b) return 1;
    if (a.length === 0 && b.length === 0) return 1;
    if (a.length === 0 || b.length === 0) return 0;

    const longer = a.length > b.length ? a : b;
    const shorter = a.length > b.length ? b : a;

    // 对于长文本，使用采样比较以提高性能
    if (longer.length > 1000) {
      return this.sampleSimilarity(longer, shorter);
    }

    const distance = this.editDistance(longer, shorter);
    return (longer.length - distance) / longer.length;
  }

  /**
   * 计算重要性评分
   *
   * @param entry - 条目
   * @returns 评分（1-4）
   */
  importanceScore(entry: SharedContextEntry): number {
    return IMPORTANCE_SCORES[entry.importance];
  }

  /**
   * 获取指定 Mission 的所有条目数量
   *
   * @param missionId - 任务 ID
   * @returns 条目数量
   */
  getEntryCount(missionId: string): number {
    let count = 0;
    for (const entry of this.entries.values()) {
      if (entry.missionId === missionId) {
        count++;
      }
    }
    return count;
  }

  /**
   * 清理指定 Mission 的所有条目
   *
   * @param missionId - 任务 ID
   * @returns 清理的条目数量
   */
  clearMission(missionId: string): number {
    let cleared = 0;
    for (const [id, entry] of this.entries.entries()) {
      if (entry.missionId === missionId) {
        if (this.removeEntryByIdInternal(id, entry)) {
          cleared++;
        }
      }
    }
    return cleared;
  }

  /**
   * 清理所有过期条目
   *
   * @returns 清理的条目数量
   */
  clearExpired(): number {
    const now = Date.now();
    let cleared = 0;
    for (const [id, entry] of this.entries.entries()) {
      if (entry.expiresAt && entry.expiresAt < now) {
        if (this.removeEntryByIdInternal(id, entry)) {
          cleared++;
        }
      }
    }
    return cleared;
  }

  /**
   * 根据 ID 获取条目
   *
   * @param id - 条目 ID
   * @returns 条目（如果存在）
   */
  getById(id: string): SharedContextEntry | undefined {
    return this.entries.get(id);
  }

  /**
   * 根据 ID 删除条目
   *
   * @param id - 条目 ID
   * @returns 是否成功删除
   */
  deleteById(id: string): boolean {
    return this.removeEntryByIdInternal(id);
  }

  /**
   * 获取所有条目的总数
   *
   * @returns 总条目数
   */
  get size(): number {
    return this.entries.size;
  }

  // --------------------------------------------------------------------------
  // 序列化 / 反序列化（支持崩溃恢复时还原 L1 共享记忆）
  // --------------------------------------------------------------------------

  /**
   * 将当前所有条目序列化为 JSON 可存储的数组
   *
   * 用于 Mission 生命周期管理器在进程崩溃前持久化 L1 记忆。
   */
  toSerializable(): SharedContextEntry[] {
    return Array.from(this.entries.values());
  }

  /**
   * 从序列化数据恢复条目（合并模式：不清空现有数据）
   *
   * 用于 Mission 恢复时重建 L1 共享记忆。
   * 采用合并策略：已存在的 id 跳过，新 id 直接插入（跳过去重校验以保证速度）。
   *
   * @param entries - 序列化的条目数组
   * @returns 恢复的条目数量
   */
  fromSerializable(entries: SharedContextEntry[]): number {
    let restored = 0;
    for (const entry of entries) {
      if (!entry.id || this.entries.has(entry.id)) {
        continue;
      }
      this.entries.set(entry.id, entry);
      this.indexEntry(entry);
      restored++;
    }
    if (restored > 0) {
      logger.info('共享上下文.反序列化.完成', { restored, total: this.entries.size }, LogCategory.SESSION);
    }
    return restored;
  }

  // --------------------------------------------------------------------------
  // 私有方法
  // --------------------------------------------------------------------------

  /**
   * 构建 mission + type 组合键
   */
  private buildMissionTypeKey(missionId: string, type: SharedContextEntryType): string {
    return `${missionId}::${type}`;
  }

  /**
   * 去重归一化（统一大小写与空白，降低无意义差异）
   */
  private normalizeDedupContent(content: string): string {
    return content.toLowerCase().replace(/\s+/g, ' ').trim();
  }

  /**
   * FNV-1a 32-bit 哈希，生成稳定短签名
   */
  private hashString(value: string): string {
    let hash = 0x811c9dc5;
    for (let i = 0; i < value.length; i++) {
      hash ^= value.charCodeAt(i);
      hash = Math.imul(hash, 0x01000193);
    }
    return (hash >>> 0).toString(36);
  }

  /**
   * 内容长度分桶
   */
  private dedupLengthBucket(contentLength: number): number {
    return Math.max(0, Math.floor(contentLength / DEDUP_LENGTH_BUCKET_SIZE));
  }

  /**
   * 构建内容签名键集合
   *
   * includeAdjacentLengthBuckets=true 时，会包含长度桶的相邻桶（-1/+1），
   * 用于“长度轻微变化”的重复检测召回。
   */
  private buildDedupSignatureKeys(
    missionTypeKey: string,
    normalizedContent: string,
    includeAdjacentLengthBuckets: boolean,
  ): string[] {
    const prefixSample = normalizedContent.slice(0, DEDUP_PREFIX_SAMPLE_SIZE);
    const tokenSample = normalizedContent
      .split(' ')
      .filter(Boolean)
      .slice(0, DEDUP_TOKEN_SAMPLE_COUNT)
      .join('|');
    const lengthBucket = this.dedupLengthBucket(normalizedContent.length);

    const keys = new Set<string>([
      `${missionTypeKey}|h|${this.hashString(normalizedContent)}`,
      `${missionTypeKey}|p|${this.hashString(prefixSample)}`,
      `${missionTypeKey}|t|${this.hashString(tokenSample || normalizedContent)}`,
      `${missionTypeKey}|l|${lengthBucket}`,
    ]);

    if (includeAdjacentLengthBuckets) {
      keys.add(`${missionTypeKey}|l|${Math.max(0, lengthBucket - 1)}`);
      keys.add(`${missionTypeKey}|l|${lengthBucket + 1}`);
    }

    return Array.from(keys);
  }

  /**
   * 从一级/二级索引中收集重复候选
   */
  private collectDuplicateCandidateIds(entry: SharedContextEntry): Set<string> {
    const missionTypeKey = this.buildMissionTypeKey(entry.missionId, entry.type);
    const normalizedContent = this.normalizeDedupContent(entry.content);
    const signatureKeys = this.buildDedupSignatureKeys(missionTypeKey, normalizedContent, true);
    const candidateIds = new Set<string>();

    for (const signatureKey of signatureKeys) {
      const bucket = this.dedupSignatureIndex.get(signatureKey);
      if (!bucket) continue;
      for (const id of bucket) {
        candidateIds.add(id);
      }
    }

    // 兜底：若二级索引暂无候选，则退回一级索引保障召回率
    if (candidateIds.size === 0) {
      const missionTypeBucket = this.missionTypeIndex.get(missionTypeKey);
      if (missionTypeBucket) {
        for (const id of missionTypeBucket) {
          candidateIds.add(id);
        }
      }
    }

    return candidateIds;
  }

  /**
   * 将条目写入索引
   */
  private indexEntry(entry: SharedContextEntry): void {
    const missionTypeKey = this.buildMissionTypeKey(entry.missionId, entry.type);
    this.addToIndex(this.missionTypeIndex, missionTypeKey, entry.id);

    const normalizedContent = this.normalizeDedupContent(entry.content);
    const signatureKeys = this.buildDedupSignatureKeys(missionTypeKey, normalizedContent, false);
    for (const signatureKey of signatureKeys) {
      this.addToIndex(this.dedupSignatureIndex, signatureKey, entry.id);
    }
  }

  /**
   * 将条目从索引移除
   */
  private deindexEntry(entry: SharedContextEntry): void {
    const missionTypeKey = this.buildMissionTypeKey(entry.missionId, entry.type);
    this.removeFromIndex(this.missionTypeIndex, missionTypeKey, entry.id);

    const normalizedContent = this.normalizeDedupContent(entry.content);
    const signatureKeys = this.buildDedupSignatureKeys(missionTypeKey, normalizedContent, false);
    for (const signatureKey of signatureKeys) {
      this.removeFromIndex(this.dedupSignatureIndex, signatureKey, entry.id);
    }
  }

  /**
   * 索引通用添加
   */
  private addToIndex(index: Map<string, Set<string>>, key: string, entryId: string): void {
    let bucket = index.get(key);
    if (!bucket) {
      bucket = new Set<string>();
      index.set(key, bucket);
    }
    bucket.add(entryId);
  }

  /**
   * 索引通用移除（自动清理空桶）
   */
  private removeFromIndex(index: Map<string, Set<string>>, key: string, entryId: string): void {
    const bucket = index.get(key);
    if (!bucket) return;

    bucket.delete(entryId);
    if (bucket.size === 0) {
      index.delete(key);
    }
  }

  /**
   * 删除条目（含索引清理）
   */
  private removeEntryByIdInternal(id: string, knownEntry?: SharedContextEntry): boolean {
    const entry = knownEntry || this.entries.get(id);
    if (!entry) {
      return false;
    }

    this.deindexEntry(entry);
    return this.entries.delete(id);
  }

  /**
   * 验证条目是否合法
   *
   * 规则：
   * 1. 内容不能超过 2000 字符（约 500 tokens）
   * 2. 必须包含来源和时间戳
   * 3. 必须包含 missionId
   */
  private validate(entry: SharedContextEntry): ValidationResult {
    if (!entry.id || typeof entry.id !== 'string') {
      return {
        allowed: false,
        reason: '缺少唯一标识 (id)'
      };
    }

    if (!entry.type || !VALID_TYPES.includes(entry.type)) {
      return {
        allowed: false,
        reason: `非法条目类型 (type=${entry.type})`
      };
    }

    if (!entry.missionId || typeof entry.missionId !== 'string' || !entry.missionId.trim()) {
      return {
        allowed: false,
        reason: '缺少任务标识 (missionId)'
      };
    }

    if (!entry.source || !VALID_SOURCES.includes(entry.source)) {
      return {
        allowed: false,
        reason: `缺少或非法来源标识 (source=${entry.source})`
      };
    }

    if (!VALID_IMPORTANCE.includes(entry.importance)) {
      return {
        allowed: false,
        reason: `非法重要性级别 (importance=${entry.importance})`
      };
    }

    if (!Array.isArray(entry.tags)) {
      return {
        allowed: false,
        reason: '缺少标签集合 (tags)'
      };
    }
    if (entry.tags.some(tag => typeof tag !== 'string' || !tag.trim())) {
      return {
        allowed: false,
        reason: '标签包含非法值 (tags)'
      };
    }

    // 规则 1: 内容长度限制
    if (!entry.content || !entry.content.trim()) {
      return {
        allowed: false,
        reason: '内容为空 (content)'
      };
    }
    if (entry.content.length > MAX_CONTENT_LENGTH) {
      return {
        allowed: false,
        reason: `内容过长 (${entry.content.length} > ${MAX_CONTENT_LENGTH})，请摘要后写入`
      };
    }

    // 规则 2: 必须带时间戳
    if (typeof entry.createdAt !== 'number' || !Number.isFinite(entry.createdAt) || entry.createdAt <= 0) {
      return {
        allowed: false,
        reason: '缺少或非法时间戳 (createdAt)'
      };
    }

    if (entry.expiresAt !== undefined) {
      if (typeof entry.expiresAt !== 'number' || !Number.isFinite(entry.expiresAt)) {
        return {
          allowed: false,
          reason: '失效时间非法 (expiresAt)'
        };
      }
      if (entry.expiresAt <= entry.createdAt) {
        return {
          allowed: false,
          reason: '失效时间必须晚于创建时间 (expiresAt <= createdAt)'
        };
      }
    }

    // 规则 3: file_summary 必须带 fileRefs
    if (entry.type === 'file_summary') {
      if (!entry.fileRefs || entry.fileRefs.length === 0) {
        return {
          allowed: false,
          reason: 'file_summary 缺少 fileRefs'
        };
      }
      if (!this.isValidFileRefs(entry.fileRefs)) {
        return {
          allowed: false,
          reason: 'fileRefs 格式非法'
        };
      }
      return { allowed: true };
    }

    // 非 file_summary 条目，如提供 fileRefs 也必须合法
    if (entry.fileRefs && !this.isValidFileRefs(entry.fileRefs)) {
      return {
        allowed: false,
        reason: 'fileRefs 格式非法'
      };
    }

    return { allowed: true };
  }

  /**
   * 校验文件引用
   */
  private isValidFileRefs(fileRefs: FileReference[]): boolean {
    return fileRefs.every(ref =>
      ref &&
      typeof ref.path === 'string' &&
      ref.path.trim().length > 0 &&
      typeof ref.hash === 'string' &&
      ref.hash.trim().length > 0
    );
  }

  /**
   * 检查条目是否满足最小重要性要求
   */
  private meetsImportance(entry: SharedContextEntry, minImportance: ImportanceLevel): boolean {
    return this.importanceScore(entry) >= IMPORTANCE_SCORES[minImportance];
  }

  /**
   * 检查条目是否匹配任一订阅标签
   */
  private matchesTags(entry: SharedContextEntry, subscribedTags: string[]): boolean {
    if (!entry.tags || entry.tags.length === 0) return false;
    return entry.tags.some(tag => subscribedTags.includes(tag));
  }

  /**
   * 按 Token 预算裁剪结果
   *
   * 从高重要性到低重要性依次添加，直到达到预算上限
   */
  private trimByTokenBudget(
    entries: SharedContextEntry[],
    maxTokens: number
  ): SharedContextEntry[] {
    const result: SharedContextEntry[] = [];
    let currentTokens = 0;

    for (const entry of entries) {
      const entryTokens = this.estimateTokens(entry.content);
      if (currentTokens + entryTokens > maxTokens) {
        // 超出预算，停止添加
        break;
      }
      result.push(entry);
      currentTokens += entryTokens;
    }

    return result;
  }

  /**
   * Token 估算（约 4 字符 = 1 token，中文约 2 字符 = 1 token）
   *
   * 采用混合估算策略，对中文字符加权
   */
  private estimateTokens(text: string): number {
    return estimateMixedLanguageTokenCount(text);
  }

  /**
   * 计算编辑距离（Levenshtein Distance）
   *
   * 使用动态规划优化空间复杂度为 O(min(m,n))
   */
  private editDistance(a: string, b: string): number {
    // 确保 a 是较短的字符串
    if (a.length > b.length) {
      [a, b] = [b, a];
    }

    const m = a.length;
    const n = b.length;

    // 只使用两行的 DP 数组
    let prev = new Array(m + 1);
    let curr = new Array(m + 1);

    // 初始化第一行
    for (let i = 0; i <= m; i++) {
      prev[i] = i;
    }

    // 填充 DP 表
    for (let j = 1; j <= n; j++) {
      curr[0] = j;
      for (let i = 1; i <= m; i++) {
        if (a[i - 1] === b[j - 1]) {
          curr[i] = prev[i - 1];
        } else {
          curr[i] = 1 + Math.min(prev[i - 1], prev[i], curr[i - 1]);
        }
      }
      [prev, curr] = [curr, prev];
    }

    return prev[m];
  }

  /**
   * 采样相似度计算（用于长文本）
   *
   * 对长文本进行分段采样比较，提高性能
   */
  private sampleSimilarity(longer: string, shorter: string): number {
    const sampleSize = 200;
    const sampleCount = 5;

    // 采样位置
    const positions = [0, 0.25, 0.5, 0.75, 1].map(ratio =>
      Math.floor(ratio * (shorter.length - sampleSize))
    );

    let totalSimilarity = 0;
    let validSamples = 0;

    for (let i = 0; i < sampleCount; i++) {
      const start = Math.max(0, positions[i]);
      const shorterSample = shorter.substring(start, start + sampleSize);

      // 在 longer 中查找最相似的位置
      let maxSim = 0;
      for (let j = 0; j < longer.length - sampleSize; j += sampleSize / 2) {
        const longerSample = longer.substring(j, j + sampleSize);
        const sim = this.calculateQuickSimilarity(shorterSample, longerSample);
        maxSim = Math.max(maxSim, sim);
      }

      totalSimilarity += maxSim;
      validSamples++;
    }

    return validSamples > 0 ? totalSimilarity / validSamples : 0;
  }

  /**
   * 快速相似度计算（基于字符频率）
   *
   * 用于采样比较时的快速估算
   */
  private calculateQuickSimilarity(a: string, b: string): number {
    const freqA = this.charFrequency(a);
    const freqB = this.charFrequency(b);

    let intersection = 0;
    let union = 0;

    const allChars = new Set([...Object.keys(freqA), ...Object.keys(freqB)]);
    for (const char of allChars) {
      const countA = freqA[char] || 0;
      const countB = freqB[char] || 0;
      intersection += Math.min(countA, countB);
      union += Math.max(countA, countB);
    }

    return union > 0 ? intersection / union : 0;
  }

  /**
   * 计算字符频率
   */
  private charFrequency(str: string): Record<string, number> {
    const freq: Record<string, number> = {};
    for (const char of str) {
      freq[char] = (freq[char] || 0) + 1;
    }
    return freq;
  }
}

// ============================================================================
// 工具函数
// ============================================================================

/**
 * 生成唯一 ID
 *
 * 格式：scp_{timestamp}_{random}
 */
export function generateSharedContextId(): string {
  const timestamp = Date.now().toString(36);
  const random = Math.random().toString(36).substring(2, 8);
  return `scp_${timestamp}_${random}`;
}

/**
 * 创建共享上下文条目
 *
 * 工厂函数，提供便捷的条目创建方式
 */
export function createSharedContextEntry(params: {
  missionId: string;
  source: ContextSource;
  type: SharedContextEntryType;
  content: string;
  tags?: string[];
  fileRefs?: FileReference[];
  importance?: ImportanceLevel;
  expiresAt?: number;
}): SharedContextEntry {
  return {
    id: generateSharedContextId(),
    missionId: params.missionId,
    source: params.source,
    type: params.type,
    content: params.content,
    tags: params.tags || [],
    fileRefs: params.fileRefs,
    importance: params.importance || 'medium',
    createdAt: Date.now(),
    expiresAt: params.expiresAt
  };
}
