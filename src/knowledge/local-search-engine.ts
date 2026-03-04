/**
 * LocalSearchEngine — 本地代码搜索引擎
 *
 * 编排所有索引和搜索策略：
 * - Sprint 1: 倒排索引 + TF-IDF（本文件）
 * - Sprint 2: 符号索引 + 依赖图谱 + 多维排序
 * - Sprint 3: LLM 查询扩展 + 搜索缓存 + 增量更新
 *
 * 设计原则：
 * - 每个组件独立可用，不可用时自动降级
 * - 搜索策略并行执行
 * - 统一的 SearchResult 输出格式
 */

import * as fs from 'fs';
import * as path from 'path';
import { InvertedIndex, IndexSearchHit } from './indexing/inverted-index';
import { CodeTokenizer } from './indexing/code-tokenizer';
import { SymbolIndex, SymbolSearchHit } from './indexing/symbol-index';
import { DependencyGraph } from './indexing/dependency-graph';
import { ResultRanker, RankedResult, FileTimestamps, RankWeights, RankBoostSignals } from './search/result-ranker';
export type { RankWeights } from './search/result-ranker';

/** 搜索引擎配置 */
export interface SearchEngineConfig {
  /** 排序权重覆盖 */
  rankWeights?: Partial<RankWeights>;
}
import { SearchCache } from './search/search-cache';
import { QueryExpander } from './search/query-expander';
import { SemanticReranker } from './search/semantic-reranker';
import { IndexPersistence } from './persistence/index-persistence';
import { LLMClient } from '../llm/types';
import { logger, LogCategory } from '../logging';

// ============================================================================
// 类型定义
// ============================================================================

/** 搜索选项 */
export interface SearchOptions {
  /** 最大返回结果数 */
  maxResults?: number;
  /** 最大上下文长度（字符数） */
  maxContextLength?: number;
  /** 是否启用 LLM 查询扩展（Sprint 3） */
  enableLLMExpansion?: boolean;
  /** 当前任务聚焦目录/文件（用于排序加权） */
  preferredScopes?: string[];
  /** 是否启用“最近编辑文件”加权（默认 true） */
  preferRecentEdits?: boolean;
}

/** 代码片段 */
export interface CodeSnippet {
  startLine: number;
  endLine: number;
  content: string;
  matchedTokens: string[];
}

/** 各维度得分明细 */
export interface ScoreBreakdown {
  tfidf: number;
  symbolMatch: number;
  positionWeight: number;
  centrality: number;
  recency: number;
  typeWeight: number;
  recentEditBoost: number;
  scopeBoost: number;
}

/** 单个搜索结果 */
export interface SearchResult {
  filePath: string;
  score: number;
  snippets: CodeSnippet[];
  scoreBreakdown: ScoreBreakdown;
}

// ============================================================================
// LocalSearchEngine 类
// ============================================================================

export class LocalSearchEngine {
  private projectRoot: string;
  private invertedIndex: InvertedIndex;
  private symbolIndex: SymbolIndex;
  private dependencyGraph: DependencyGraph;
  private resultRanker: ResultRanker;
  private searchCache: SearchCache<SearchResult[]>;
  private queryExpander: QueryExpander;
  private semanticReranker: SemanticReranker;
  private tokenizer: CodeTokenizer;
  private persistence: IndexPersistence;
  /** 当前已索引的文件列表（用于持久化保存） */
  private _indexedFiles: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }> = [];
  private _isReady = false;
  /** 增量更新串行队列（确保文件事件按顺序应用） */
  private pendingMutationQueue: Promise<void> = Promise.resolve();
  /** 文件状态快照（用于查询前一致性对账） */
  private trackedFileStates = new Map<string, { mtimeMs: number; size: number }>();
  /** 索引版本（每次文件变更或对账修复递增） */
  private indexVersion = 0;
  /** 项目词表脏标记（索引变更后在查询前刷新） */
  private projectVocabularyDirty = true;
  /** 最近编辑文件记录（filePath -> timestamp） */
  private recentEditedFiles = new Map<string, number>();
  private static readonly RECENT_EDIT_TTL_MS = 30 * 60 * 1000;
  private static readonly RECENT_EDIT_MAX_FILES = 200;

  constructor(projectRoot: string, config?: SearchEngineConfig) {
    this.projectRoot = projectRoot;
    this.invertedIndex = new InvertedIndex();
    this.symbolIndex = new SymbolIndex();
    this.dependencyGraph = new DependencyGraph();
    this.resultRanker = new ResultRanker(config?.rankWeights);
    this.searchCache = new SearchCache<SearchResult[]>();
    this.queryExpander = new QueryExpander();
    this.semanticReranker = new SemanticReranker();
    this.tokenizer = new CodeTokenizer();
    this.persistence = new IndexPersistence(projectRoot);
  }

  get isReady(): boolean {
    return this._isReady;
  }

  /**
   * 设置 LLM 客户端（辅助模型，用于查询扩展 + 语义重排序）
   */
  setLLMClient(client: LLMClient | null): void {
    this.queryExpander.setLLMClient(client);
    this.semanticReranker.setLLMClient(client);
    this.semanticReranker.setSymbolIndex(this.symbolIndex);
  }

  /**
   * 构建索引（优先从持久化缓存恢复，否则全量构建）
   */
  async buildIndex(
    files: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>
  ): Promise<void> {
    this._indexedFiles = [...files];
    const startTime = Date.now();

    try {
      // 先等待已排队的增量变更落地，避免与重建并发交叉
      await this.pendingMutationQueue;

      // 尝试从持久化缓存恢复 + 增量同步
      const restoreResult = this.persistence.restoreAndSync(
        this.projectRoot,
        this.invertedIndex,
        this.symbolIndex,
        this.dependencyGraph,
        files
      );

      if (restoreResult.restored) {
        // 恢复 LLM 扩展缓存
        if (restoreResult.expansionCache) {
          this.queryExpander.importCache(restoreResult.expansionCache);
        }
        this.rebuildTrackedFileStates();
        this.refreshProjectVocabularyIfNeeded();
        this.bumpIndexVersion('restore');
        this._isReady = true;
        const elapsed = Date.now() - startTime;
        logger.info('本地搜索引擎.从缓存恢复', {
          elapsed: `${elapsed}ms`,
          stats: this.getStats(),
        }, LogCategory.SESSION);

        // 恢复后立即保存（包含增量同步的更新）
        this.persistence.debouncedSave(
          this.projectRoot, this.invertedIndex, this.symbolIndex,
          this.dependencyGraph, this._indexedFiles,
          this.queryExpander.exportCache()
        );

        return;
      }

      // 缓存不可用，全量构建
      const [indexResult, symbolResult, depResult] = await Promise.allSettled([
        this.invertedIndex.buildFromFiles(this.projectRoot, files),
        this.symbolIndex.buildFromFiles(this.projectRoot, files),
        this.dependencyGraph.buildFromFiles(this.projectRoot, files),
      ]);

      this._isReady = true;
      this.rebuildTrackedFileStates();
      this.refreshProjectVocabularyIfNeeded();
      this.bumpIndexVersion('rebuild');
      const elapsed = Date.now() - startTime;
      const indexStats = this.invertedIndex.getStats();
      const symbolStats = this.symbolIndex.getStats();
      const depStats = this.dependencyGraph.getStats();

      logger.info('本地搜索引擎.索引构建完成', {
        files: indexStats.totalDocuments,
        uniqueTokens: indexStats.uniqueTokens,
        symbols: symbolStats.uniqueSymbols,
        depEdges: depStats.totalEdges,
        elapsed: `${elapsed}ms`,
        failures: [indexResult, symbolResult, depResult]
          .filter(r => r.status === 'rejected').length,
      }, LogCategory.SESSION);

      // 全量构建完成后保存到磁盘
      this.persistence.save(
        this.projectRoot, this.invertedIndex, this.symbolIndex,
        this.dependencyGraph, this._indexedFiles,
        this.queryExpander.exportCache()
      );
    } catch (error) {
      logger.warn('本地搜索引擎.索引构建失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 搜索入口
   */
  async search(query: string, options: SearchOptions = {}): Promise<SearchResult[]> {
    const {
      maxResults = 10,
      maxContextLength = 8000,
      enableLLMExpansion = false,
      preferredScopes = [],
      preferRecentEdits = true,
    } = options;

    if (!query.trim()) return [];

    // 0. 查询前强一致：等待增量队列 + 文件状态对账修复
    const consistencyStart = Date.now();
    const reconciled = await this.ensureConsistencyBeforeSearch();
    if (reconciled > 0) {
      logger.info('本地搜索引擎.查询前一致性修复', {
        reconciled,
        elapsed: `${Date.now() - consistencyStart}ms`,
        indexVersion: this.indexVersion,
      }, LogCategory.SESSION);
    }

    // 1. 缓存命中检查
    const cached = this.searchCache.get(query);
    if (cached) {
      logger.info('本地搜索引擎.缓存命中', {
        query: query.substring(0, 50),
        results: cached.length,
        indexVersion: this.indexVersion,
      }, LogCategory.SESSION);
      return cached;
    }

    const startTime = Date.now();

    // 2. 查询意图检测
    const queryIntent = this.detectQueryIntent(query);

    // 3. 分词 + 查询扩展
    const queryTokens = this.tokenizer.tokenizeQuery(query);
    if (queryTokens.length === 0) return [];

    let searchTokens = queryTokens;
    let expansionMode = 'none';
    let weightHints: Partial<RankWeights> | undefined;

    // 符号查询跳过 LLM 扩展（避免噪声和不必要的延迟）
    const shouldExpand = queryIntent === 'semantic'
      && (enableLLMExpansion || queryTokens.length <= 3);
    if (shouldExpand) {
      try {
        const expanded = await this.queryExpander.expand(query, queryTokens);
        searchTokens = expanded.expandedTokens;
        expansionMode = expanded.mode;
        weightHints = expanded.weightHints;
      } catch {
        // 扩展失败，使用原始 token
      }
    }

    // 3.5 并行执行多源搜索
    //     SymbolIndex 对 searchTokens 逐个搜索（而非原始 query），
    //     使查询扩展后的同义词也能命中符号索引
    const [indexHitsResult, symbolHitsResult] = await Promise.allSettled([
      Promise.resolve(this.invertedIndex.isReady
        ? this.invertedIndex.search(searchTokens, maxResults * 3)
        : []),
      Promise.resolve(this.symbolIndex.isReady
        ? this.symbolIndex.searchMulti(searchTokens, maxResults * 2, query)
        : []),
    ]);

    const indexHits = indexHitsResult.status === 'fulfilled' ? indexHitsResult.value : [];
    const symbolHits = symbolHitsResult.status === 'fulfilled' ? symbolHitsResult.value : [];

    // 4. 构建文件时间戳映射（用于 recency 评分）
    const fileTimestamps: FileTimestamps = {
      get: (filePath: string) => this.invertedIndex.getDocumentMeta(filePath)?.lastModified,
    };
    const boostSignals: RankBoostSignals = {
      preferredScopes: this.normalizeScopeHints(preferredScopes),
      recentEditedFiles: preferRecentEdits ? this.getRecentEditedFileSet() : undefined,
    };

    // 5. 多维融合排序（按查询意图调整权重）
    //    优先级：symbol 意图硬编码权重 > LLM 意图分析 weightHints > 默认权重
    const weightOverrides: Partial<RankWeights> | undefined =
      queryIntent === 'symbol'
        ? { symbolMatch: 0.50, tfidf: 0.20, positionWeight: 0.12, centrality: 0.08, recency: 0.05, typeWeight: 0.05 }
        : weightHints;
    const ranked = this.resultRanker.rank(
      indexHits,
      symbolHits,
      this.dependencyGraph.isReady ? this.dependencyGraph : null,
      maxResults * 2,
      fileTimestamps,
      weightOverrides,
      boostSignals
    );

    // 6. 依赖图上下文扩展：对 Top-3 结果沿依赖关系展开 1 层，
    //    追加关联文件（以 0.5× 衰减分加入）
    const expandedRanked = this.expandWithDependencies(ranked, maxResults * 2);

    // 7. 语义重排序：辅助模型对 Top 候选进行语义精排
    const reranked = await this.semanticReranker.rerank(query, expandedRanked);

    // 8. 组装搜索结果 + 提取代码片段（异步并行预加载文件内容）
    const results = await this.assembleRankedResults(reranked, indexHits, symbolHits, maxResults, maxContextLength);

    // 9. 写入缓存（附带结果涉及的文件路径，用于精细化失效）
    const resultFilePaths = new Set(results.map(r => r.filePath));
    this.searchCache.set(query, results, resultFilePaths);

    const elapsed = Date.now() - startTime;
    logger.info('本地搜索引擎.搜索完成', {
      query: query.substring(0, 50),
      tokens: queryTokens.length,
      expandedTokens: searchTokens.length,
      expansionMode,
      indexHits: indexHits.length,
      symbolHits: symbolHits.length,
      rankedResults: ranked.length,
      results: results.length,
      elapsed: `${elapsed}ms`,
      indexVersion: this.indexVersion,
    }, LogCategory.SESSION);

    return results;
  }

  /**
   * 增量更新单个文件
   */
  onFileChanged(filePath: string): void {
    if (!this._isReady) return;
    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.projectRoot, filePath)
      : filePath;
    this.enqueueMutation(`changed:${relativePath}`, () => {
      const fileType = this.ensureIndexedFileRecord(relativePath);
      const changed = this.applyChangedFile(relativePath, fileType);
      if (changed > 0) {
        this.refreshProjectVocabularyIfNeeded();
        this.bumpIndexVersion(`changed:${relativePath}`);
        this.persistence.debouncedSave(
          this.projectRoot, this.invertedIndex, this.symbolIndex,
          this.dependencyGraph, this._indexedFiles,
          this.queryExpander.exportCache()
        );
      }
    });
  }

  /**
   * 新增文件
   */
  onFileCreated(filePath: string): void {
    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.projectRoot, filePath)
      : filePath;
    this.enqueueMutation(`created:${relativePath}`, () => {
      const fileType = this.ensureIndexedFileRecord(relativePath, this.classifyFileType(relativePath));
      const changed = this.applyChangedFile(relativePath, fileType);
      if (changed > 0) {
        this.refreshProjectVocabularyIfNeeded();
        this.bumpIndexVersion(`created:${relativePath}`);
        this.persistence.debouncedSave(
          this.projectRoot, this.invertedIndex, this.symbolIndex,
          this.dependencyGraph, this._indexedFiles,
          this.queryExpander.exportCache()
        );
      }
    });
  }

  /**
   * 删除文件
   */
  onFileDeleted(filePath: string): void {
    if (!this._isReady) return;
    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.projectRoot, filePath)
      : filePath;
    this.enqueueMutation(`deleted:${relativePath}`, () => {
      const changed = this.applyDeletedFile(relativePath);
      if (changed > 0) {
        this.refreshProjectVocabularyIfNeeded();
        this.bumpIndexVersion(`deleted:${relativePath}`);
        this.persistence.debouncedSave(
          this.projectRoot, this.invertedIndex, this.symbolIndex,
          this.dependencyGraph, this._indexedFiles,
          this.queryExpander.exportCache()
        );
      }
    });
  }

  /**
   * 释放资源
   */
  dispose(): void {
    this.persistence.dispose();
  }

  /**
   * 获取引擎统计信息
   */
  getStats(): {
    isReady: boolean;
    indexStats: ReturnType<InvertedIndex['getStats']>;
    symbolStats: ReturnType<SymbolIndex['getStats']>;
    depStats: ReturnType<DependencyGraph['getStats']>;
    cacheStats: ReturnType<SearchCache<SearchResult[]>['getStats']>;
  } {
    return {
      isReady: this._isReady,
      indexStats: this.invertedIndex.getStats(),
      symbolStats: this.symbolIndex.getStats(),
      depStats: this.dependencyGraph.getStats(),
      cacheStats: this.searchCache.getStats(),
    };
  }

  /**
   * 查询前强一致保证：
   * 1) 等待增量更新队列完成
   * 2) 对 indexedFiles 做 mtime/size 对账，修复漏事件/跨语言漏监听
   */
  private async ensureConsistencyBeforeSearch(): Promise<number> {
    if (!this._isReady) return 0;
    await this.pendingMutationQueue;
    const reconciled = this.reconcileIndexedFiles();
    this.refreshProjectVocabularyIfNeeded();
    if (reconciled > 0) {
      this.bumpIndexVersion(`reconcile:${reconciled}`);
      this.persistence.debouncedSave(
        this.projectRoot, this.invertedIndex, this.symbolIndex,
        this.dependencyGraph, this._indexedFiles,
        this.queryExpander.exportCache()
      );
    }
    return reconciled;
  }

  /**
   * 串行化增量变更，避免并发写索引导致中间态可见
   */
  private enqueueMutation(label: string, mutation: () => void): void {
    const run = (): void => {
      try {
        mutation();
      } catch (error) {
        logger.warn('本地搜索引擎.增量变更执行失败', { label, error }, LogCategory.SESSION);
      }
    };
    this.pendingMutationQueue = this.pendingMutationQueue.then(run, run);
  }

  /**
   * 对账 indexedFiles 与文件系统状态
   * 返回修复文件数量
   */
  private reconcileIndexedFiles(): number {
    let changedCount = 0;
    const indexedSnapshot = [...this._indexedFiles];
    const stillIndexed = new Set<string>();

    for (const entry of indexedSnapshot) {
      const relativePath = entry.path;
      stillIndexed.add(relativePath);
      const fullPath = path.join(this.projectRoot, relativePath);

      let stat: fs.Stats | null = null;
      try {
        stat = fs.statSync(fullPath);
        if (!stat.isFile()) {
          stat = null;
        }
      } catch {
        stat = null;
      }

      if (!stat) {
        changedCount += this.applyDeletedFile(relativePath);
        continue;
      }

      const tracked = this.trackedFileStates.get(relativePath);
      const drifted = !tracked
        || Math.abs(tracked.mtimeMs - stat.mtimeMs) > 1
        || tracked.size !== stat.size;

      if (drifted) {
        changedCount += this.applyChangedFile(relativePath, entry.type);
      }
    }

    // 清理不再索引的残留快照
    for (const trackedPath of Array.from(this.trackedFileStates.keys())) {
      if (!stillIndexed.has(trackedPath)) {
        this.trackedFileStates.delete(trackedPath);
      }
    }

    return changedCount;
  }

  /**
   * 变更文件：刷新三类索引 + 快照
   */
  private applyChangedFile(
    relativePath: string,
    fileType: 'source' | 'config' | 'doc' | 'test'
  ): number {
    this.invertedIndex.updateFile(this.projectRoot, relativePath, fileType);
    this.symbolIndex.updateFile(this.projectRoot, relativePath);
    this.dependencyGraph.updateFile(this.projectRoot, relativePath);
    this.updateTrackedFileState(relativePath);
    this.recordRecentEdit(relativePath);
    this.projectVocabularyDirty = true;
    this.searchCache.invalidateByFile(relativePath);
    return 1;
  }

  /**
   * 删除文件：移除索引和文件清单
   */
  private applyDeletedFile(relativePath: string): number {
    let changed = 0;

    const prevLength = this._indexedFiles.length;
    this._indexedFiles = this._indexedFiles.filter(f => f.path !== relativePath);
    if (this._indexedFiles.length !== prevLength) changed++;

    if (this.trackedFileStates.delete(relativePath)) changed++;
    this.recentEditedFiles.delete(relativePath);
    this.projectVocabularyDirty = true;

    this.invertedIndex.removeFile(relativePath);
    this.symbolIndex.removeFile(relativePath);
    this.dependencyGraph.removeFile(relativePath);
    this.searchCache.invalidateByFile(relativePath);

    return changed > 0 ? 1 : 0;
  }

  /**
   * 确保文件存在于索引文件清单，并返回文件类型
   */
  private ensureIndexedFileRecord(
    relativePath: string,
    preferredType?: 'source' | 'config' | 'doc' | 'test'
  ): 'source' | 'config' | 'doc' | 'test' {
    const existing = this._indexedFiles.find(f => f.path === relativePath);
    if (existing) {
      if (preferredType && existing.type !== preferredType) {
        existing.type = preferredType;
      }
      return existing.type;
    }
    const type = preferredType ?? this.classifyFileType(relativePath);
    this._indexedFiles.push({ path: relativePath, type });
    return type;
  }

  /**
   * 重建文件状态快照（用于对账）
   */
  private rebuildTrackedFileStates(): void {
    this.trackedFileStates.clear();
    for (const file of this._indexedFiles) {
      this.updateTrackedFileState(file.path);
    }
    this.projectVocabularyDirty = true;
  }

  /**
   * 更新单文件状态快照
   */
  private updateTrackedFileState(relativePath: string): void {
    const fullPath = path.join(this.projectRoot, relativePath);
    try {
      const stat = fs.statSync(fullPath);
      if (!stat.isFile()) {
        this.trackedFileStates.delete(relativePath);
        return;
      }
      this.trackedFileStates.set(relativePath, {
        mtimeMs: stat.mtimeMs,
        size: stat.size,
      });
    } catch {
      this.trackedFileStates.delete(relativePath);
    }
  }

  /**
   * 递增索引版本并全量失效缓存，确保检索一致性
   */
  private bumpIndexVersion(reason: string): void {
    this.indexVersion += 1;
    this.searchCache.invalidateAll();
    logger.debug('本地搜索引擎.索引版本更新', {
      version: this.indexVersion,
      reason,
    }, LogCategory.SESSION);
  }

  /**
   * 刷新 QueryExpander 项目词表（脏标记驱动）
   */
  private refreshProjectVocabularyIfNeeded(): void {
    if (!this.projectVocabularyDirty) return;
    const vocabulary = this.buildProjectVocabulary();
    this.queryExpander.setProjectVocabulary(vocabulary);
    this.projectVocabularyDirty = false;
  }

  /**
   * 聚合符号名 + 文件路径片段，构建项目词表
   */
  private buildProjectVocabulary(): Set<string> {
    const vocabulary = this.symbolIndex.getVocabulary();
    for (const file of this._indexedFiles) {
      this.addPathTokensToVocabulary(file.path, vocabulary);
    }
    return vocabulary;
  }

  /**
   * 从路径中抽取可检索词（lowercase）
   */
  private addPathTokensToVocabulary(filePath: string, vocabulary: Set<string>): void {
    const normalized = filePath.replace(/\\/g, '/');
    const parts = normalized.split(/[\/._-]+/).filter(Boolean);
    for (const part of parts) {
      const token = part.trim().toLowerCase();
      if (token.length >= 3 && token.length <= 64 && /^[a-z0-9_]+$/.test(token)) {
        vocabulary.add(token);
      }
    }
  }

  /**
   * 记录最近编辑文件（用于排序加权）
   */
  private recordRecentEdit(filePath: string): void {
    const now = Date.now();
    this.recentEditedFiles.set(filePath, now);
    if (this.recentEditedFiles.size <= LocalSearchEngine.RECENT_EDIT_MAX_FILES) {
      return;
    }
    const sorted = Array.from(this.recentEditedFiles.entries())
      .sort((a, b) => b[1] - a[1])
      .slice(0, LocalSearchEngine.RECENT_EDIT_MAX_FILES);
    this.recentEditedFiles = new Map(sorted);
  }

  /**
   * 获取有效期内的最近编辑文件集合
   */
  private getRecentEditedFileSet(): Set<string> {
    const now = Date.now();
    const recent = new Set<string>();
    for (const [filePath, timestamp] of this.recentEditedFiles.entries()) {
      if (now - timestamp <= LocalSearchEngine.RECENT_EDIT_TTL_MS) {
        recent.add(filePath);
      } else {
        this.recentEditedFiles.delete(filePath);
      }
    }
    return recent;
  }

  /**
   * 规范化 scope 路径
   */
  private normalizeScopeHints(scopePaths: string[]): string[] {
    if (!Array.isArray(scopePaths) || scopePaths.length === 0) return [];
    return scopePaths
      .filter(scope => typeof scope === 'string')
      .map(scope => scope.trim())
      .filter(Boolean)
      .slice(0, 8)
      .map(scope => scope.replace(/\\/g, '/').replace(/^\.\/+/, ''));
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  /**
   * 将融合排序后的结果组装为完整搜索结果（含代码片段提取）
   * Fix 2: 同时接收 symbolHits，当文件仅被符号索引命中时使用符号行号提取 snippets
   * 优化 #16: 批量异步并行预加载文件内容，消除搜索路径上的 readFileSync
   */
  private async assembleRankedResults(
    ranked: RankedResult[],
    indexHits: IndexSearchHit[],
    symbolHits: SymbolSearchHit[],
    maxResults: number,
    maxContextLength: number
  ): Promise<SearchResult[]> {
    // 批量异步预加载：收集候选文件路径 → Promise.all 并行读取
    const candidateFiles = new Set(ranked.slice(0, maxResults).map(r => r.filePath));
    const fileContents = new Map<string, string[]>();

    await Promise.all(
      Array.from(candidateFiles).map(async (filePath) => {
        try {
          const fullPath = path.join(this.projectRoot, filePath);
          const content = await fs.promises.readFile(fullPath, 'utf-8');
          fileContents.set(filePath, content.split('\n'));
        } catch {
          // 文件不存在或无法读取，跳过
        }
      })
    );

    const results: SearchResult[] = [];
    let totalContentLength = 0;

    // 建立 indexHits 的文件查找表
    const indexHitMap = new Map<string, IndexSearchHit>();
    for (const hit of indexHits) {
      indexHitMap.set(hit.filePath, hit);
    }

    // 建立 symbolHits 的文件 → 行号查找表
    const symbolLineMap = new Map<string, number[]>();
    for (const hit of symbolHits) {
      const lines = symbolLineMap.get(hit.symbol.filePath) || [];
      lines.push(hit.symbol.line);
      symbolLineMap.set(hit.symbol.filePath, lines);
    }

    for (const rankedItem of ranked) {
      if (results.length >= maxResults) break;
      if (totalContentLength >= maxContextLength) break;

      const lines = fileContents.get(rankedItem.filePath);
      if (!lines) continue; // 文件预加载失败，跳过

      let snippets: CodeSnippet[] = [];

      // 策略 1: 优先使用 indexHit 的行号信息
      const indexHit = indexHitMap.get(rankedItem.filePath);
      if (indexHit) {
        snippets = this.extractSnippets(rankedItem.filePath, lines, indexHit.hitLines, indexHit.matchedTokens, maxContextLength - totalContentLength);
      }

      // 策略 2: 无 indexHit 时，使用符号定义行号
      if (snippets.length === 0) {
        const symbolLines = symbolLineMap.get(rankedItem.filePath);
        if (symbolLines && symbolLines.length > 0) {
          snippets = this.extractSnippets(rankedItem.filePath, lines, symbolLines, [], maxContextLength - totalContentLength);
        }
      }

      const result: SearchResult = {
        filePath: rankedItem.filePath,
        score: rankedItem.finalScore,
        snippets,
        scoreBreakdown: {
          tfidf: rankedItem.breakdown.tfidf,
          symbolMatch: rankedItem.breakdown.symbolMatch,
          positionWeight: rankedItem.breakdown.positionWeight,
          centrality: rankedItem.breakdown.centrality,
          recency: rankedItem.breakdown.recency,
          typeWeight: rankedItem.breakdown.typeWeight,
          recentEditBoost: rankedItem.breakdown.recentEditBoost,
          scopeBoost: rankedItem.breakdown.scopeBoost,
        },
      };

      results.push(result);
      totalContentLength += snippets.reduce((sum, s) => sum + s.content.length, 0);
    }

    return results;
  }

  /**
   * 依赖图上下文扩展
   * Fix 5: 对 Top-3 命中文件沿依赖关系展开 1 层，以 0.5× 衰减分追加关联文件
   */
  private expandWithDependencies(ranked: RankedResult[], maxResults: number): RankedResult[] {
    if (!this.dependencyGraph.isReady || ranked.length === 0) return ranked;

    const existingFiles = new Set(ranked.map(r => r.filePath));
    const expanded: RankedResult[] = [...ranked];
    const topN = Math.min(3, ranked.length);

    for (let i = 0; i < topN; i++) {
      const topResult = ranked[i];
      // 展开 1 层依赖（正向 + 反向）
      const neighbors = this.dependencyGraph.expand(topResult.filePath, 1, 'both');

      for (const neighborFile of neighbors) {
        if (existingFiles.has(neighborFile)) continue;
        existingFiles.add(neighborFile);

        expanded.push({
          filePath: neighborFile,
          finalScore: topResult.finalScore * 0.5, // 衰减 50%
          breakdown: {
            tfidf: 0,
            symbolMatch: 0,
            positionWeight: 0,
            centrality: this.dependencyGraph.getCentrality(neighborFile),
            recency: 0,
            typeWeight: 0,
            recentEditBoost: 0,
            scopeBoost: 0,
          },
          sources: ['dependency'],
        });
      }
    }

    return expanded
      .sort((a, b) => b.finalScore - a.finalScore)
      .slice(0, maxResults);
  }

  /**
   * 从预加载的文件内容中按行号列表提取代码片段
   * 利用 SymbolIndex 的符号边界提取完整代码块（函数/类/方法），
   * 非符号区域（import/全局代码）使用上下文窗口
   * 优化 #16: 接收预加载的 lines，不再自行读取文件
   */
  private extractSnippets(filePath: string, lines: string[], hitLines: number[], matchedTokens: string[], maxLength: number): CodeSnippet[] {
    const snippets: CodeSnippet[] = [];

    try {
      let totalLength = 0;

      // 1. 将每个 hitLine 映射到代码块范围
      const ranges: Array<{ startLine: number; endLine: number }> = [];

      for (const hitLine of hitLines) {
        let startLine: number;
        let endLine: number;

        // 代码块级索引: 查找包含该行的符号边界
        const symbol = this.symbolIndex.isReady
          ? this.symbolIndex.getSymbolAtLine(filePath, hitLine)
          : null;

        if (symbol && symbol.endLine !== undefined && symbol.endLine > symbol.line) {
          // 符号区域 → 使用符号边界
          startLine = symbol.line;
          endLine = symbol.endLine;

          // 大代码块截断：单个块最多 50 行，以 hitLine 为中心
          if (endLine - startLine + 1 > 50) {
            startLine = Math.max(symbol.line, hitLine - 25);
            endLine = Math.min(symbol.endLine, hitLine + 25);
          }
        } else {
          // 非符号区域（import/全局代码/配置行）→ 上下文窗口
          startLine = Math.max(0, hitLine - 2);
          endLine = Math.min(lines.length - 1, hitLine + 2);
        }

        ranges.push({ startLine, endLine });
      }

      // 2. 按起始行排序 → 合并重叠范围
      ranges.sort((a, b) => a.startLine - b.startLine);
      const merged: Array<{ startLine: number; endLine: number }> = [];
      for (const range of ranges) {
        const last = merged[merged.length - 1];
        if (last && range.startLine <= last.endLine + 1) {
          // 与上一个范围重叠或相邻 → 合并
          last.endLine = Math.max(last.endLine, range.endLine);
        } else {
          merged.push({ ...range });
        }
      }

      // 3. 按合并后的范围提取代码片段
      for (const range of merged) {
        if (totalLength >= maxLength) break;

        const snippetContent = lines.slice(range.startLine, range.endLine + 1).join('\n');
        if (totalLength + snippetContent.length > maxLength) break;

        snippets.push({
          startLine: range.startLine,
          endLine: range.endLine,
          content: snippetContent,
          matchedTokens,
        });
        totalLength += snippetContent.length;
      }
    } catch {
      // 文件读取失败
    }

    return snippets;
  }

  /**
   * 文件类型分类
   * 覆盖 SymbolIndex 支持的所有语言扩展名，以及常见配置和测试文件
   */
  private classifyFileType(filePath: string): 'source' | 'config' | 'doc' | 'test' {
    const ext = path.extname(filePath).toLowerCase();
    const baseName = path.basename(filePath).toLowerCase();

    // 测试文件
    if (baseName.includes('.test.') || baseName.includes('.spec.')
        || filePath.includes('/test/') || filePath.includes('/tests/')
        || filePath.includes('/__tests__/')) {
      return 'test';
    }

    // 配置文件
    const configExts = new Set(['.json', '.yaml', '.yml', '.toml', '.ini', '.env', '.cfg']);
    if (configExts.has(ext)) return 'config';

    // 源码文件（与 SymbolIndex LANG_PATTERNS 对齐）
    const sourceExts = new Set([
      '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs',
      '.py', '.go', '.java', '.rs',
      '.c', '.h', '.cpp', '.cc', '.cxx', '.hpp', '.hh',
      '.cs', '.php', '.rb', '.swift', '.kt', '.kts',
      '.m', '.mm',
      '.vue', '.svelte',
    ]);
    if (sourceExts.has(ext)) return 'source';

    return 'doc';
  }

  /**
   * 查询意图检测
   * - 'symbol': 查询看起来像符号名（PascalCase/camelCase/snake_case，无空格/中文）
   * - 'semantic': 自然语言查询（含空格、中文等）
   */
  private detectQueryIntent(query: string): 'symbol' | 'semantic' {
    const trimmed = query.trim();
    // 符号模式：纯标识符字符，无空格，无中文
    if (/^[a-zA-Z_$][a-zA-Z0-9_$]*$/.test(trimmed)) {
      return 'symbol';
    }
    // snake_case 连写
    if (/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(trimmed) && trimmed.includes('_')) {
      return 'symbol';
    }
    return 'semantic';
  }
}
