/**
 * SemanticReranker — 语义重排序器
 *
 * 利用辅助模型（Auxiliary LLM）对候选结果进行语义重排序。
 * 作用于检索流水线末端：ResultRanker 排序后、代码片段组装前。
 *
 * 设计原则：
 * - 辅助模型不可用时静默跳过，返回原排序（零影响降级）
 * - 输入量控制在 ~300 token，输出 ~50 token（haiku 级模型甜区）
 * - 带缓存 + 超时保护
 */

import { LLMClient } from '../../llm/types';
import { logger, LogCategory } from '../../logging';
import type { RankedResult } from './result-ranker';
import type { SymbolIndex } from '../indexing/symbol-index';

/** 重排序缓存条目 */
interface RerankerCacheEntry {
  reorderedPaths: string[];
  timestamp: number;
}

export class SemanticReranker {
  private llmClient: LLMClient | null = null;
  private symbolIndex: SymbolIndex | null = null;
  private cache = new Map<string, RerankerCacheEntry>();
  private static readonly CACHE_MAX = 30;
  private static readonly CACHE_TTL = 120_000;
  private static readonly LLM_TIMEOUT = 5_000;

  setLLMClient(client: LLMClient | null): void { this.llmClient = client; }
  setSymbolIndex(idx: SymbolIndex): void { this.symbolIndex = idx; }

  /**
   * 对候选结果进行语义重排序
   * @param query 用户原始查询
   * @param candidates ResultRanker 排序后的候选列表
   * @param topN 取前 N 个候选进行重排（剩余保持原序追加）
   */
  async rerank(query: string, candidates: RankedResult[], topN = 15): Promise<RankedResult[]> {
    if (!this.llmClient || candidates.length <= 2) return candidates;

    const rerankSlice = candidates.slice(0, Math.min(topN, candidates.length));
    const restSlice = candidates.slice(rerankSlice.length);

    const cacheKey = this.buildCacheKey(query, rerankSlice);
    const cached = this.cache.get(cacheKey);
    if (cached && (Date.now() - cached.timestamp) < SemanticReranker.CACHE_TTL) {
      return this.applyReorder(rerankSlice, restSlice, cached.reorderedPaths);
    }

    try {
      const summary = this.buildCandidateSummary(rerankSlice);
      const prompt =
`你是代码搜索结果排序器。根据用户查询意图，对候选文件重新排序。

查询: "${query}"

候选文件:
${summary}

输出: 按相关性从高到低排列的文件序号（逗号分隔，如: 3,1,5,2,4）。只输出序号，无其他文字。`;

      const response = await this.callWithTimeout(prompt);
      if (!response) return candidates;

      const reorderedPaths = this.parseReorderResponse(response, rerankSlice);
      if (reorderedPaths.length === 0) return candidates;

      this.cacheSet(cacheKey, reorderedPaths);
      logger.info('语义重排序.完成', {
        query: query.substring(0, 50),
        candidates: rerankSlice.length,
        reordered: reorderedPaths.length,
      }, LogCategory.SESSION);
      return this.applyReorder(rerankSlice, restSlice, reorderedPaths);
    } catch (error) {
      logger.debug('语义重排序.降级_返回原排序', {
        query: query.substring(0, 50),
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.SESSION);
      return candidates;
    }
  }

  private buildCandidateSummary(candidates: RankedResult[]): string {
    return candidates.map((c, i) => {
      const symbols = this.getTopSymbols(c.filePath, 5);
      const symStr = symbols.length > 0 ? ` [${symbols.join(', ')}]` : '';
      return `${i + 1}. ${c.filePath}${symStr}`;
    }).join('\n');
  }

  private getTopSymbols(filePath: string, max: number): string[] {
    if (!this.symbolIndex?.isReady) return [];
    const symbols = this.symbolIndex.getSymbolsForFile(filePath);
    if (!symbols || symbols.length === 0) return [];
    return symbols
      .sort((a, b) => (a.isExported !== b.isExported ? (a.isExported ? -1 : 1) : 0))
      .slice(0, max)
      .map(s => s.name);
  }

  private async callWithTimeout(prompt: string): Promise<string | null> {
    if (!this.llmClient) return null;
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), SemanticReranker.LLM_TIMEOUT);
    try {
      const res = await this.llmClient.sendMessage({
        messages: [{ role: 'user', content: prompt }],
        maxTokens: 100, temperature: 0.1, signal: ctrl.signal,
      });
      return res?.content || null;
    } catch (e) {
      if (e instanceof Error && e.name === 'AbortError') {
        logger.debug('语义重排序.超时', undefined, LogCategory.SESSION);
      }
      return null;
    } finally { clearTimeout(timer); }
  }

  private parseReorderResponse(response: string, candidates: RankedResult[]): string[] {
    const nums = response.match(/\d+/g);
    if (!nums || nums.length === 0) return [];
    const seen = new Set<number>();
    const paths: string[] = [];
    for (const n of nums) {
      const idx = parseInt(n, 10) - 1;
      if (idx >= 0 && idx < candidates.length && !seen.has(idx)) {
        seen.add(idx);
        paths.push(candidates[idx].filePath);
      }
    }
    if (paths.length < candidates.length * 0.5) return [];
    for (let i = 0; i < candidates.length; i++) {
      if (!seen.has(i)) paths.push(candidates[i].filePath);
    }
    return paths;
  }

  private applyReorder(slice: RankedResult[], rest: RankedResult[], paths: string[]): RankedResult[] {
    const map = new Map<string, RankedResult>();
    for (const c of slice) map.set(c.filePath, c);
    const maxScore = slice.length > 0 ? slice[0].finalScore : 1;
    const reordered: RankedResult[] = [];
    for (let i = 0; i < paths.length; i++) {
      const c = map.get(paths[i]);
      if (c) reordered.push({ ...c, finalScore: maxScore * (1 - i * 0.02) });
    }
    return [...reordered, ...rest];
  }

  private buildCacheKey(query: string, candidates: RankedResult[]): string {
    return `${query.trim().toLowerCase()}::${candidates.map(c => c.filePath).join('|')}`;
  }

  private cacheSet(key: string, paths: string[]): void {
    if (this.cache.has(key)) this.cache.delete(key);
    while (this.cache.size >= SemanticReranker.CACHE_MAX) {
      const fk = this.cache.keys().next().value;
      if (fk !== undefined) this.cache.delete(fk);
    }
    this.cache.set(key, { reorderedPaths: paths, timestamp: Date.now() });
  }
}