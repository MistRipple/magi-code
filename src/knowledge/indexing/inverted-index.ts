/**
 * InvertedIndex — 倒排索引
 *
 * 基于 TF-IDF 的代码全文检索索引：
 * - 构建：文件内容 → CodeTokenizer 分词 → 倒排表
 * - 查询：关键词 → O(1) 查表 → TF-IDF 评分排序 → Top-K 结果
 * - 增量：支持单文件级别的添加/删除/更新
 */

import * as fs from 'fs';
import * as path from 'path';
import { CodeTokenizer, FileTokenResult, TokenContext } from './code-tokenizer';
import { MinHeap } from '../utils/min-heap';
import { logger, LogCategory } from '../../logging';

// ============================================================================
// 类型定义
// ============================================================================

/** token 在文件中的出现位置 */
export interface TokenPosition {
  line: number;
  column: number;
  context: TokenContext;
}

/** 倒排表中的发布条目 */
export interface PostingEntry {
  /** 文件相对路径 */
  filePath: string;
  /** 该 token 在此文件中的出现次数 */
  frequency: number;
  /** 出现位置列表 */
  positions: TokenPosition[];
}

/** 文件元数据 */
export interface DocumentMeta {
  /** 文件中的 token 总数 */
  totalTokens: number;
  /** 最后修改时间 */
  lastModified: number;
  /** 文件类型 */
  fileType: 'source' | 'config' | 'doc' | 'test';
}

/** 单个搜索命中 */
export interface IndexSearchHit {
  filePath: string;
  score: number;
  /** 最高权重的上下文类型 */
  bestContext: TokenContext;
  /** 匹配到的 token 列表 */
  matchedTokens: string[];
  /** 命中的行号列表（去重后前 10 个） */
  hitLines: number[];
}

/** 倒排索引序列化快照 */
export interface InvertedIndexSnapshot {
  index: Array<[string, PostingEntry[]]>;
  documentFrequency: Array<[string, number]>;
  documentMeta: Array<[string, DocumentMeta]>;
  totalDocuments: number;
}

// ============================================================================
// InvertedIndex 类
// ============================================================================

export class InvertedIndex {
  /** token → 发布列表 */
  private index = new Map<string, PostingEntry[]>();
  /** token → 包含该 token 的文件数 */
  private documentFrequency = new Map<string, number>();
  /** 文件路径 → 元数据 */
  private documentMeta = new Map<string, DocumentMeta>();
  /** 索引中的文件总数 */
  private totalDocuments = 0;
  /** 所有文档的平均 token 数（BM25 需要） */
  private avgDocLength = 0;
  /** 所有文档的 token 总数（增量维护，避免每次重算） */
  private totalTokenSum = 0;
  /** 分词器 */
  private tokenizer = new CodeTokenizer();
  /** 是否已构建 */
  private _isReady = false;

  // BM25 参数
  private static readonly BM25_K1 = 1.2;   // 词频饱和参数
  private static readonly BM25_B = 0.75;   // 文档长度归一化参数

  get isReady(): boolean {
    return this._isReady;
  }

  /**
   * 批量构建索引
   */
  async buildFromFiles(
    projectRoot: string,
    files: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>
  ): Promise<void> {
    const startTime = Date.now();
    this.clear();

    const BATCH_SIZE = 50; // 每批处理 50 个文件后让出事件循环
    for (let i = 0; i < files.length; i++) {
      const file = files[i];
      try {
        const fullPath = path.join(projectRoot, file.path);
        if (!fs.existsSync(fullPath)) continue;

        const stat = fs.statSync(fullPath);
        // 跳过大文件（> 500KB）
        if (stat.size > 500 * 1024) continue;

        const content = fs.readFileSync(fullPath, 'utf-8');
        const tokenResult = this.tokenizer.tokenizeFile(file.path, content);
        this.addDocument(file.path, tokenResult, stat.mtimeMs, file.type);
      } catch {
        // 跳过无法读取的文件
      }

      // 每处理 BATCH_SIZE 个文件后让出事件循环，避免阻塞 UI
      if ((i + 1) % BATCH_SIZE === 0) {
        await new Promise<void>(resolve => setImmediate(resolve));
      }
    }

    this._isReady = true;
    const elapsed = Date.now() - startTime;
    logger.info('倒排索引.构建完成', {
      files: this.totalDocuments,
      tokens: this.index.size,
      elapsed: `${elapsed}ms`,
    }, LogCategory.SESSION);
  }

  /**
   * 搜索：返回按 BM25 排序的文件列表
   * 优化 #1: BM25 替代朴素 TF-IDF
   * 优化 #2: 查询词覆盖率加分
   * 优化 #5: 邻近性加分（Proximity Boost）
   */
  search(queryTokens: string[], maxResults = 20): IndexSearchHit[] {
    if (!this._isReady || queryTokens.length === 0) return [];

    const { BM25_K1, BM25_B } = InvertedIndex;
    const uniqueQueryTokens = [...new Set(queryTokens.map(t => t.toLowerCase()))];

    // 收集所有命中的文件及其各维度信息
    const fileScores = new Map<string, {
      totalScore: number;
      matchedTokens: Set<string>;
      bestContext: TokenContext;
      hitLines: Set<number>;
      /** 各 token 命中的行号集合（用于邻近性检测） */
      tokenLineMap: Map<string, Set<number>>;
    }>();

    for (const token of uniqueQueryTokens) {
      const postings = this.index.get(token);
      if (!postings) continue;

      const idf = this.calculateIDF(token);

      for (const posting of postings) {
        const meta = this.documentMeta.get(posting.filePath);
        if (!meta) continue;

        // BM25 评分公式
        const tf = posting.frequency;
        const dl = meta.totalTokens;
        const bm25Score = idf * (tf * (BM25_K1 + 1))
          / (tf + BM25_K1 * (1 - BM25_B + BM25_B * dl / this.avgDocLength));

        let entry = fileScores.get(posting.filePath);
        if (!entry) {
          entry = {
            totalScore: 0,
            matchedTokens: new Set(),
            bestContext: 'usage',
            hitLines: new Set(),
            tokenLineMap: new Map(),
          };
          fileScores.set(posting.filePath, entry);
        }

        entry.totalScore += bm25Score;
        entry.matchedTokens.add(token);

        // 记录该 token 命中的行号（用于邻近性检测）
        const tokenLines = entry.tokenLineMap.get(token) || new Set<number>();
        for (const pos of posting.positions) {
          tokenLines.add(pos.line);
        }
        entry.tokenLineMap.set(token, tokenLines);

        // 更新最高上下文权重
        const contextPriority: Record<TokenContext, number> = {
          definition: 4, import: 3, usage: 2, string: 1, comment: 0,
        };
        for (const pos of posting.positions) {
          entry.hitLines.add(pos.line);
          if (contextPriority[pos.context] > contextPriority[entry.bestContext]) {
            entry.bestContext = pos.context;
          }
        }
      }
    }

    // 优化 #2: 查询词覆盖率加分
    // 优化 #5: 邻近性加分
    // 优化 #18: 遍历中追踪 maxScore，避免额外 Map 展开
    let maxScore = 0.001;
    for (const [, entry] of fileScores) {
      // 覆盖率加分：匹配的查询词越多，额外奖励越大
      if (uniqueQueryTokens.length > 1) {
        const coverage = entry.matchedTokens.size / uniqueQueryTokens.length;
        // 100% 覆盖 → +20%，50% 覆盖 → +5%
        entry.totalScore *= (1 + coverage * 0.2);
      }

      // 邻近性加分：多个查询词出现在相邻 3 行内 → 额外 +30%
      if (entry.tokenLineMap.size >= 2) {
        const proximityBoost = this.calculateProximityBoost(entry.tokenLineMap);
        entry.totalScore *= (1 + proximityBoost);
      }

      if (entry.totalScore > maxScore) maxScore = entry.totalScore;
    }

    // 优化 #17: MinHeap Top-K 替换全量 sort+slice
    const heap = new MinHeap<IndexSearchHit>(maxResults, (a, b) => a.score - b.score);

    for (const [filePath, entry] of fileScores.entries()) {
      heap.push({
        filePath,
        score: entry.totalScore / maxScore,
        bestContext: entry.bestContext,
        matchedTokens: Array.from(entry.matchedTokens),
        hitLines: Array.from(entry.hitLines).sort((a, b) => a - b).slice(0, 10),
      });
    }

    return heap.toSortedDescArray();
  }

  // ==========================================================================
  // 增量更新
  // ==========================================================================

  /**
   * 添加/更新单个文件的索引
   */
  updateFile(
    projectRoot: string,
    filePath: string,
    fileType: 'source' | 'config' | 'doc' | 'test' = 'source'
  ): void {
    // 先删除旧索引
    this.removeFile(filePath);

    try {
      const fullPath = path.join(projectRoot, filePath);
      if (!fs.existsSync(fullPath)) return;

      const stat = fs.statSync(fullPath);
      if (stat.size > 500 * 1024) return;

      const content = fs.readFileSync(fullPath, 'utf-8');
      const tokenResult = this.tokenizer.tokenizeFile(filePath, content);
      this.addDocument(filePath, tokenResult, stat.mtimeMs, fileType);
    } catch {
      // 跳过无法读取的文件
    }
  }

  /**
   * 从索引中删除文件
   */
  removeFile(filePath: string): void {
    if (!this.documentMeta.has(filePath)) return;

    // 遍历所有 token，删除该文件的 posting
    for (const [token, postings] of this.index.entries()) {
      const filtered = postings.filter(p => p.filePath !== filePath);
      if (filtered.length === 0) {
        this.index.delete(token);
        this.documentFrequency.delete(token);
      } else {
        this.index.set(token, filtered);
        this.documentFrequency.set(token, filtered.length);
      }
    }

    const removedMeta = this.documentMeta.get(filePath);
    if (removedMeta) {
      this.totalTokenSum -= removedMeta.totalTokens;
    }
    this.documentMeta.delete(filePath);
    this.totalDocuments--;
    this.avgDocLength = this.totalDocuments > 0 ? this.totalTokenSum / this.totalDocuments : 0;
  }

  /**
   * 清空索引
   */
  clear(): void {
    this.index.clear();
    this.documentFrequency.clear();
    this.documentMeta.clear();
    this.totalDocuments = 0;
    this.avgDocLength = 0;
    this.totalTokenSum = 0;
    this._isReady = false;
  }

  /**
   * 获取索引统计信息
   */
  getStats(): { totalDocuments: number; uniqueTokens: number; isReady: boolean } {
    return {
      totalDocuments: this.totalDocuments,
      uniqueTokens: this.index.size,
      isReady: this._isReady,
    };
  }

  /**
   * 获取文件的元数据（用于 recency 等外部评分）
   */
  getDocumentMeta(filePath: string): DocumentMeta | undefined {
    return this.documentMeta.get(filePath);
  }

  /**
   * 获取所有已索引文件的元数据（用于持久化新鲜度校验）
   */
  getAllDocumentMeta(): Map<string, DocumentMeta> {
    return new Map(this.documentMeta);
  }

  // ==========================================================================
  // 序列化 / 反序列化
  // ==========================================================================

  /**
   * 序列化索引为 JSON 可存储对象
   */
  toJSON(): InvertedIndexSnapshot {
    return {
      index: Array.from(this.index.entries()),
      documentFrequency: Array.from(this.documentFrequency.entries()),
      documentMeta: Array.from(this.documentMeta.entries()),
      totalDocuments: this.totalDocuments,
    };
  }

  /**
   * 从序列化数据恢复索引
   */
  fromJSON(snapshot: InvertedIndexSnapshot): void {
    this.clear();
    this.index = new Map(snapshot.index);
    this.documentFrequency = new Map(snapshot.documentFrequency);
    this.documentMeta = new Map(snapshot.documentMeta);
    this.totalDocuments = snapshot.totalDocuments;
    this.recalcAvgDocLength();
    this._isReady = true;
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  /**
   * 添加单个文档到索引
   */
  private addDocument(
    filePath: string,
    tokenResult: FileTokenResult,
    lastModified: number,
    fileType: 'source' | 'config' | 'doc' | 'test'
  ): void {
    // 存储文件元数据
    this.documentMeta.set(filePath, {
      totalTokens: tokenResult.totalTokens,
      lastModified,
      fileType,
    });
    this.totalDocuments++;
    this.totalTokenSum += tokenResult.totalTokens;
    this.avgDocLength = this.totalTokenSum / this.totalDocuments;

    // 按 token 聚合位置信息
    const tokenPositions = new Map<string, TokenPosition[]>();
    for (const tw of tokenResult.tokens) {
      const positions = tokenPositions.get(tw.token) || [];
      positions.push({ line: tw.line, column: tw.column, context: tw.context });
      tokenPositions.set(tw.token, positions);
    }

    // 更新倒排索引
    for (const [token, positions] of tokenPositions.entries()) {
      const postings = this.index.get(token) || [];
      postings.push({
        filePath,
        frequency: positions.length,
        positions,
      });
      this.index.set(token, postings);
      this.documentFrequency.set(token, (this.documentFrequency.get(token) || 0) + 1);
    }
  }

  /**
   * 计算 IDF（逆文档频率）
   */
  private calculateIDF(token: string): number {
    const df = this.documentFrequency.get(token) || 0;
    return Math.log((this.totalDocuments + 1) / (df + 1)) + 1;
  }

  /**
   * 从序列化数据恢复时重新计算 totalTokenSum 和 avgDocLength
   */
  private recalcAvgDocLength(): void {
    this.totalTokenSum = 0;
    for (const meta of this.documentMeta.values()) {
      this.totalTokenSum += meta.totalTokens;
    }
    this.avgDocLength = this.totalDocuments > 0 ? this.totalTokenSum / this.totalDocuments : 0;
  }

  /**
   * 计算邻近性加分（优化 #5）
   * 多个查询词出现在相邻行（距离 ≤ 3）→ 最高 +30%
   */
  /**
   * 优化 #19: 预排序 tokenLineMap，避免循环内重复 Array.from().sort()
   */
  private calculateProximityBoost(tokenLineMap: Map<string, Set<number>>): number {
    // 预排序：每个 Set → sorted number[]（仅排序一次）
    const sortedLineSets: number[][] = [];
    for (const lineSet of tokenLineMap.values()) {
      sortedLineSets.push(Array.from(lineSet).sort((a, b) => a - b));
    }
    if (sortedLineSets.length < 2) return 0;

    let bestProximity = Infinity;

    // 对每对 token 计算最小行距
    for (let i = 0; i < sortedLineSets.length; i++) {
      for (let j = i + 1; j < sortedLineSets.length; j++) {
        const linesA = sortedLineSets[i];
        const linesB = sortedLineSets[j];

        // 双指针求最小距离
        let ai = 0, bi = 0;
        while (ai < linesA.length && bi < linesB.length) {
          const dist = Math.abs(linesA[ai] - linesB[bi]);
          bestProximity = Math.min(bestProximity, dist);
          if (bestProximity === 0) break;
          if (linesA[ai] < linesB[bi]) ai++;
          else bi++;
        }
        if (bestProximity === 0) break;
      }
      if (bestProximity === 0) break;
    }

    // 行距 0（同一行）→ 0.30，行距 1 → 0.25，行距 2 → 0.20，行距 3 → 0.10，>3 → 0
    if (bestProximity === 0) return 0.30;
    if (bestProximity === 1) return 0.25;
    if (bestProximity === 2) return 0.20;
    if (bestProximity <= 3) return 0.10;
    return 0;
  }
}