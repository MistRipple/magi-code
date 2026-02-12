/**
 * ResultRanker — 多维融合排序器
 *
 * 将不同搜索策略的结果按多维度评分进行融合排序：
 * - TF-IDF 文本相关性
 * - 符号匹配精确度
 * - 上下文位置权重（definition > import > usage > comment）
 * - 依赖图中心度
 * - 文件新鲜度（recency）
 * - 文件类型权重
 *
 * 每个维度的权重可配置。
 */

import { IndexSearchHit } from '../indexing/inverted-index';
import { SymbolSearchHit } from '../indexing/symbol-index';
import { DependencyGraph } from '../indexing/dependency-graph';
import { MinHeap } from '../utils/min-heap';

// ============================================================================
// 类型定义
// ============================================================================

/** 融合后的排序结果 */
export interface RankedResult {
  filePath: string;
  /** 最终综合得分（0-1） */
  finalScore: number;
  /** 各维度得分明细 */
  breakdown: ScoreDimensions;
  /** 来源标记 */
  sources: ('index' | 'symbol' | 'dependency')[];
}

/** 各维度得分 */
export interface ScoreDimensions {
  /** 倒排索引 TF-IDF 得分 */
  tfidf: number;
  /** 符号匹配得分 */
  symbolMatch: number;
  /** 上下文位置权重 */
  positionWeight: number;
  /** 依赖图中心度 */
  centrality: number;
  /** 文件新鲜度（最近修改的文件得分更高） */
  recency: number;
  /** 文件类型权重 */
  typeWeight: number;
}

/** 排序权重配置 */
export interface RankWeights {
  tfidf: number;
  symbolMatch: number;
  positionWeight: number;
  centrality: number;
  recency: number;
  typeWeight: number;
}

/** 文件时间信息（由调用方传入） */
export interface FileTimestamps {
  /** 文件路径 → 最后修改时间戳(ms) */
  get(filePath: string): number | undefined;
}

/** 聚合条目（消除 any 类型） */
interface FileScoreEntry {
  sources: Set<string>;
  tfidf: number;
  symbolMatch: number;
  positionWeight: number;
  centrality: number;
  recency: number;
  typeWeight: number;
}

/** 默认权重 */
const DEFAULT_WEIGHTS: RankWeights = {
  tfidf: 0.30,
  symbolMatch: 0.28,
  positionWeight: 0.15,
  centrality: 0.10,
  recency: 0.07,
  typeWeight: 0.10,
};

// ============================================================================
// ResultRanker 类
// ============================================================================

export class ResultRanker {
  private weights: RankWeights;

  constructor(weights?: Partial<RankWeights>) {
    this.weights = { ...DEFAULT_WEIGHTS, ...weights };
    // 归一化权重
    const total = Object.values(this.weights).reduce((sum, w) => sum + w, 0);
    if (total > 0) {
      for (const key of Object.keys(this.weights) as Array<keyof RankWeights>) {
        this.weights[key] /= total;
      }
    }
  }

  /**
   * 融合多源搜索结果并排序
   * @param fileTimestamps 文件最后修改时间映射（用于 recency 计算）
   */
  rank(
    indexHits: IndexSearchHit[],
    symbolHits: SymbolSearchHit[],
    dependencyGraph: DependencyGraph | null,
    maxResults = 20,
    fileTimestamps?: FileTimestamps
  ): RankedResult[] {
    // 按文件聚合所有来源的分数
    const fileMap = new Map<string, FileScoreEntry>();

    // 1. 处理倒排索引结果
    for (const hit of indexHits) {
      const entry = this.getOrCreate(fileMap, hit.filePath);
      entry.sources.add('index');
      entry.tfidf = Math.max(entry.tfidf, hit.score);
      entry.positionWeight = Math.max(
        entry.positionWeight,
        this.contextToWeight(hit.bestContext)
      );
    }

    // 2. 处理符号匹配结果
    for (const hit of symbolHits) {
      const entry = this.getOrCreate(fileMap, hit.symbol.filePath);
      entry.sources.add('symbol');
      entry.symbolMatch = Math.max(entry.symbolMatch, hit.score);
      // 导出符号额外加分
      if (hit.symbol.isExported) {
        entry.typeWeight = Math.max(entry.typeWeight, 0.3);
      }
    }

    // 3. 补充依赖图信息
    if (dependencyGraph?.isReady) {
      for (const [filePath, entry] of fileMap.entries()) {
        entry.centrality = dependencyGraph.getCentrality(filePath);
      }
    }

    // 4. 补充文件新鲜度（recency）
    if (fileTimestamps) {
      const now = Date.now();
      for (const [filePath, entry] of fileMap.entries()) {
        const mtime = fileTimestamps.get(filePath);
        if (mtime) {
          entry.recency = this.calculateRecency(mtime, now);
        }
      }
    }

    // 优化 #17: MinHeap Top-K 替换全量 sort+slice
    const heap = new MinHeap<RankedResult>(maxResults, (a, b) => a.finalScore - b.finalScore);

    for (const [filePath, entry] of fileMap.entries()) {
      const breakdown: ScoreDimensions = {
        tfidf: entry.tfidf,
        symbolMatch: entry.symbolMatch,
        positionWeight: entry.positionWeight,
        centrality: entry.centrality,
        recency: entry.recency,
        typeWeight: entry.typeWeight,
      };

      let finalScore =
        breakdown.tfidf * this.weights.tfidf +
        breakdown.symbolMatch * this.weights.symbolMatch +
        breakdown.positionWeight * this.weights.positionWeight +
        breakdown.centrality * this.weights.centrality +
        breakdown.recency * this.weights.recency +
        breakdown.typeWeight * this.weights.typeWeight;

      // 优化 #7: 多来源交叉加分
      // 同时被 2 个来源命中 → +10%，3 个来源 → +20%
      if (entry.sources.size >= 3) {
        finalScore *= 1.20;
      } else if (entry.sources.size >= 2) {
        finalScore *= 1.10;
      }

      heap.push({
        filePath,
        finalScore,
        breakdown,
        sources: Array.from(entry.sources) as RankedResult['sources'],
      });
    }

    return heap.toSortedDescArray();
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  private getOrCreate(
    map: Map<string, FileScoreEntry>,
    filePath: string
  ): FileScoreEntry {
    let entry = map.get(filePath);
    if (!entry) {
      entry = {
        sources: new Set<string>(),
        tfidf: 0,
        symbolMatch: 0,
        positionWeight: 0,
        centrality: 0,
        recency: 0,
        typeWeight: 0,
      };
      map.set(filePath, entry);
    }
    return entry;
  }

  /**
   * 计算文件新鲜度得分（0-1）
   * 优化 #8: 指数衰减函数替代离散阶梯
   * 半衰期 = 72 小时（3 天），即 3 天前的文件得分约 0.5
   */
  private calculateRecency(lastModified: number, now: number): number {
    const ageHours = (now - lastModified) / (1000 * 60 * 60);
    const halfLife = 72; // 半衰期：72 小时
    return Math.pow(0.5, ageHours / halfLife);
  }

  /**
   * 上下文类型 → 权重
   */
  private contextToWeight(context: string): number {
    switch (context) {
      case 'definition': return 1.0;
      case 'import': return 0.6;
      case 'usage': return 0.4;
      case 'string': return 0.2;
      case 'comment': return 0.2;
      default: return 0.3;
    }
  }
}
