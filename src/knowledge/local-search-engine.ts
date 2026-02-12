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
import { ResultRanker, RankedResult, FileTimestamps } from './search/result-ranker';
import { SearchCache } from './search/search-cache';
import { QueryExpander } from './search/query-expander';
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
  private tokenizer: CodeTokenizer;
  private persistence: IndexPersistence;
  /** 当前已索引的文件列表（用于持久化保存） */
  private _indexedFiles: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }> = [];
  private _isReady = false;

  constructor(projectRoot: string) {
    this.projectRoot = projectRoot;
    this.invertedIndex = new InvertedIndex();
    this.symbolIndex = new SymbolIndex();
    this.dependencyGraph = new DependencyGraph();
    this.resultRanker = new ResultRanker();
    this.searchCache = new SearchCache<SearchResult[]>();
    this.queryExpander = new QueryExpander();
    this.tokenizer = new CodeTokenizer();
    this.persistence = new IndexPersistence(projectRoot);
  }

  get isReady(): boolean {
    return this._isReady;
  }

  /**
   * 设置 LLM 客户端（用于查询扩展）
   */
  setLLMClient(client: LLMClient | null): void {
    this.queryExpander.setLLMClient(client);
  }

  /**
   * 构建索引（优先从持久化缓存恢复，否则全量构建）
   */
  async buildIndex(
    files: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>
  ): Promise<void> {
    this._indexedFiles = files;
    const startTime = Date.now();

    try {
      // 尝试从持久化缓存恢复 + 增量同步
      const restored = this.persistence.restoreAndSync(
        this.projectRoot,
        this.invertedIndex,
        this.symbolIndex,
        this.dependencyGraph,
        files
      );

      if (restored) {
        this._isReady = true;
        const elapsed = Date.now() - startTime;
        logger.info('本地搜索引擎.从缓存恢复', {
          elapsed: `${elapsed}ms`,
          stats: this.getStats(),
        }, LogCategory.SESSION);

        // 恢复后立即保存（包含增量同步的更新）
        this.persistence.debouncedSave(
          this.projectRoot, this.invertedIndex, this.symbolIndex,
          this.dependencyGraph, this._indexedFiles
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
        this.dependencyGraph, this._indexedFiles
      );
    } catch (error) {
      logger.warn('本地搜索引擎.索引构建失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 搜索入口
   */
  async search(query: string, options: SearchOptions = {}): Promise<SearchResult[]> {
    const { maxResults = 10, maxContextLength = 8000, enableLLMExpansion = false } = options;

    if (!query.trim()) return [];

    // 0. 缓存命中检查
    const cached = this.searchCache.get(query);
    if (cached) {
      logger.info('本地搜索引擎.缓存命中', {
        query: query.substring(0, 50),
        results: cached.length,
      }, LogCategory.SESSION);
      return cached;
    }

    const startTime = Date.now();

    // 1. 分词 + 查询扩展
    const queryTokens = this.tokenizer.tokenizeQuery(query);
    if (queryTokens.length === 0) return [];

    let searchTokens = queryTokens;
    let expansionMode = 'none';

    if (enableLLMExpansion || queryTokens.length <= 3) {
      // 对短查询或显式启用时执行查询扩展
      try {
        const expanded = await this.queryExpander.expand(query, queryTokens);
        searchTokens = expanded.expandedTokens;
        expansionMode = expanded.mode;
      } catch {
        // 扩展失败，使用原始 token
      }
    }

    // 2. 并行执行多源搜索
    //    Fix 1: SymbolIndex 对 searchTokens 逐个搜索（而非原始 query），
    //    使查询扩展后的同义词也能命中符号索引
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

    // 3. 构建文件时间戳映射（用于 recency 评分）
    const fileTimestamps: FileTimestamps = {
      get: (filePath: string) => this.invertedIndex.getDocumentMeta(filePath)?.lastModified,
    };

    // 4. 多维融合排序
    const ranked = this.resultRanker.rank(
      indexHits,
      symbolHits,
      this.dependencyGraph.isReady ? this.dependencyGraph : null,
      maxResults * 2,
      fileTimestamps
    );

    // 5. 依赖图上下文扩展：对 Top-3 结果沿依赖关系展开 1 层，
    //    追加关联文件（以 0.5× 衰减分加入）
    const expandedRanked = this.expandWithDependencies(ranked, maxResults * 2);

    // 6. 组装搜索结果 + 提取代码片段（异步并行预加载文件内容）
    const results = await this.assembleRankedResults(expandedRanked, indexHits, symbolHits, maxResults, maxContextLength);

    // 7. 写入缓存
    this.searchCache.set(query, results);

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
    this.invertedIndex.updateFile(this.projectRoot, relativePath);
    this.symbolIndex.updateFile(this.projectRoot, relativePath);
    this.dependencyGraph.updateFile(this.projectRoot, relativePath);
    this.searchCache.invalidateAll();

    // 防抖保存持久化索引
    this.persistence.debouncedSave(
      this.projectRoot, this.invertedIndex, this.symbolIndex,
      this.dependencyGraph, this._indexedFiles
    );
  }

  /**
   * 新增文件
   */
  onFileCreated(filePath: string): void {
    // 将新文件加入文件清单
    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.projectRoot, filePath)
      : filePath;
    if (!this._indexedFiles.some(f => f.path === relativePath)) {
      const ext = path.extname(relativePath);
      const type = ['.ts', '.tsx', '.js', '.jsx'].includes(ext) ? 'source' as const : 'doc' as const;
      this._indexedFiles.push({ path: relativePath, type });
    }
    this.onFileChanged(filePath);
  }

  /**
   * 删除文件
   */
  onFileDeleted(filePath: string): void {
    if (!this._isReady) return;
    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.projectRoot, filePath)
      : filePath;
    this.invertedIndex.removeFile(relativePath);
    this.symbolIndex.removeFile(relativePath);
    this.dependencyGraph.removeFile(relativePath);
    this.searchCache.invalidateAll();

    // 从文件清单中移除
    this._indexedFiles = this._indexedFiles.filter(f => f.path !== relativePath);

    // 防抖保存
    this.persistence.debouncedSave(
      this.projectRoot, this.invertedIndex, this.symbolIndex,
      this.dependencyGraph, this._indexedFiles
    );
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

}