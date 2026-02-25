/**
 * LocalCodeSearchService — 本地代码搜索服务
 *
 * 统一封装 ACE 不可用时的三级搜索降级策略：
 * - L1: PKB.search()（TF-IDF + 符号索引 + 依赖图）
 * - L2: Grep 精确匹配
 * - L3: LSP workspace 符号搜索
 *
 * 性能优化：
 * - L1/L2/L3 全并行启动，按优先级组装结果 + 上下文长度剪枝
 * - L2 Grep 多关键词并行执行（Promise.allSettled）
 * - L3 LSP 多关键词并行执行（Promise.allSettled）
 * - LRU 结果缓存（避免相同查询重复 I/O）
 * - PKB 通过惰性引用获取，初始化前自动跳过 L1
 */

import * as crypto from 'crypto';
import { logger, LogCategory } from '../logging';
import type { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';
import { WorkspaceFolderInfo, WorkspaceRoots } from '../workspace/workspace-roots';

/** 最小执行器接口（仅需 execute 方法） */
interface Executor {
  execute(toolCall: { id: string; name: string; arguments: Record<string, any> }): Promise<{ content: string; isError?: boolean }>;
}

/**
 * 依赖注入接口
 * 通过 getter 惰性引用避免初始化时序问题和循环依赖
 */
export interface LocalCodeSearchDeps {
  /** 惰性获取知识库（PKB 可能尚未初始化） */
  getKnowledgeBase: () => ProjectKnowledgeBase | undefined;
  /** 惰性获取 grep 搜索执行器 */
  getSearchExecutor: () => Executor | undefined;
  /** 惰性获取 LSP 执行器 */
  getLspExecutor: () => Executor | undefined;
  /** 关键词提取 */
  extractKeywords: (query: string) => string[];
  /** 工作区目录列表 */
  workspaceFolders: WorkspaceFolderInfo[];
}

/** 结果缓存条目 */
interface CacheEntry {
  result: string | null;
  timestamp: number;
}

export class LocalCodeSearchService {
  private static readonly CACHE_MAX_SIZE = 30;
  private static readonly CACHE_TTL_MS = 45_000; // 45s（略短于 PKB SearchCache 的 60s，避免过期数据）

  /** LRU 结果缓存 */
  private cache = new Map<string, CacheEntry>();
  private workspaceRoots: WorkspaceRoots;

  constructor(private deps: LocalCodeSearchDeps) {
    this.workspaceRoots = new WorkspaceRoots(deps.workspaceFolders);
  }

  /** 本地搜索是否可用（PKB / Grep / LSP 任一就绪即可） */
  get isAvailable(): boolean {
    const kb = this.deps.getKnowledgeBase();
    const kbReady = !!(kb?.getSearchEngine?.()?.isReady);
    return kbReady || !!this.deps.getSearchExecutor() || !!this.deps.getLspExecutor();
  }

  /** 文件变更时失效缓存（由外部 FileWatcher 调用） */
  invalidateCache(): void {
    this.cache.clear();
  }

  /**
   * 三级搜索入口（全并行 + 结果级缓存）
   * @returns 搜索结果文本，无结果时返回 null
   */
  async search(query: string, maxResults: number = 10): Promise<string | null> {
    // 缓存命中检查
    const cacheKey = this.normalizeCacheKey(query);
    const cached = this.cache.get(cacheKey);
    if (cached && (Date.now() - cached.timestamp) < LocalCodeSearchService.CACHE_TTL_MS) {
      // LRU: 移到末尾
      this.cache.delete(cacheKey);
      this.cache.set(cacheKey, cached);
      return cached.result;
    }

    const startTime = Date.now();
    const maxContextLength = 6000;
    const keywords = this.deps.extractKeywords(query);

    // 全并行启动 L1/L2/L3，不互相等待
    const [l1Result, l2Result, l3Result] = await Promise.allSettled([
      this.pkbSearch(query, maxResults, maxContextLength),
      this.grepSearch(keywords, maxContextLength),
      this.lspSearch(keywords, maxContextLength),
    ]);

    // 按优先级组装结果 + 上下文长度剪枝
    const parts: string[] = [];
    let currentLength = 0;

    // L1: PKB 结果最高质量，优先填充
    const l1 = l1Result.status === 'fulfilled' ? l1Result.value : null;
    if (l1) {
      parts.push(l1);
      currentLength += l1.length;
    }

    // L2: Grep 补充（L1 不足 80% 时填入）
    const l2 = l2Result.status === 'fulfilled' ? l2Result.value : null;
    if (l2 && currentLength < maxContextLength * 0.8) {
      const budget = maxContextLength - currentLength;
      const trimmed = l2.length > budget ? l2.substring(0, budget) + '\n... (更多结果已省略)' : l2;
      parts.push(`## 关键词匹配\n${trimmed}`);
      currentLength += trimmed.length;
    }

    // L3: LSP 补充（L1+L2 不足 90% 时填入）
    const l3 = l3Result.status === 'fulfilled' ? l3Result.value : null;
    if (l3 && currentLength < maxContextLength * 0.9) {
      const budget = maxContextLength - currentLength;
      const trimmed = l3.length > budget ? l3.substring(0, budget) : l3;
      parts.push(`## 符号定义\n${trimmed}`);
    }

    const result = parts.length === 0
      ? null
      : `Query: "${query}"\nSearched via local index (TF-IDF + Symbol + Grep + LSP)\n\n${parts.join('\n\n')}`;

    // 写入缓存
    this.cacheSet(cacheKey, result);

    const elapsed = Date.now() - startTime;
    logger.debug('本地搜索.完成', {
      query: query.substring(0, 50),
      elapsed: `${elapsed}ms`,
      hasL1: !!l1, hasL2: !!l2, hasL3: !!l3,
      totalLength: currentLength,
    }, LogCategory.TOOLS);

    return result;
  }

  // ============================================================================
  // L1: PKB 索引搜索
  // ============================================================================

  private async pkbSearch(query: string, maxResults: number, maxContextLength: number): Promise<string | null> {
    const kb = this.deps.getKnowledgeBase();
    if (!kb) return null;

    try {
      const results = await kb.search(query, {
        maxResults,
        maxContextLength: Math.floor(maxContextLength * 0.6),
        enableLLMExpansion: true,
      });
      if (results.length === 0) return null;

      return results
        .map(r => {
          const snippetText = r.snippets
            .map(s => '```\n' + s.content + '\n```')
            .join('\n');
          return `### ${r.filePath} (score: ${r.score.toFixed(2)})\n${snippetText}`;
        })
        .join('\n\n');
    } catch (error) {
      logger.warn('本地搜索.PKB索引搜索失败', { error }, LogCategory.TOOLS);
      return null;
    }
  }

  // ============================================================================
  // L2: Grep 并行搜索
  // ============================================================================

  private async grepSearch(keywords: string[], maxLength: number): Promise<string | null> {
    const searchExecutor = this.deps.getSearchExecutor();
    if (!searchExecutor) return null;

    const searchKeywords = keywords.filter(kw => kw.length >= 3).slice(0, 3);
    if (searchKeywords.length === 0) return null;

    const maxPerKeyword = Math.floor(maxLength / searchKeywords.length);

    // 所有关键词并行执行
    const settled = await Promise.allSettled(
      searchKeywords.map(keyword => {
        const escapedKeyword = keyword.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        return searchExecutor.execute({
          id: `local-grep-${Date.now()}-${keyword}`,
          name: 'grep_search',
          arguments: {
            pattern: escapedKeyword,
            include: '*.ts,*.tsx,*.js,*.jsx,*.py,*.go,*.rs,*.java,*.c,*.cpp,*.h,*.hpp,*.cs,*.php,*.rb,*.swift,*.kt,*.m,*.vue',
            context_lines: 2,
            case_sensitive: false,
          },
        }).then(result => ({ keyword, result }));
      })
    );

    const parts: string[] = [];
    for (const entry of settled) {
      if (entry.status !== 'fulfilled') continue;
      const { keyword, result } = entry.value;
      if (result.isError || !result.content || result.content === 'No matches found') continue;
      const truncated = result.content.length > maxPerKeyword
        ? result.content.substring(0, maxPerKeyword) + '\n... (更多结果已省略)'
        : result.content;
      parts.push(`### 关键词: "${keyword}"\n${truncated}`);
    }
    return parts.length > 0 ? parts.join('\n\n') : null;
  }

  // ============================================================================
  // L3: LSP 并行搜索
  // ============================================================================

  private async lspSearch(keywords: string[], maxLength: number): Promise<string | null> {
    const lspExecutor = this.deps.getLspExecutor();
    if (!lspExecutor) return null;

    const symbolKeywords = keywords
      .filter(kw => /^[a-zA-Z_][a-zA-Z0-9_]*$/.test(kw) && kw.length >= 3)
      .slice(0, 3);
    if (symbolKeywords.length === 0) return null;

    // 所有符号关键词并行查询
    const settled = await Promise.allSettled(
      symbolKeywords.map(keyword =>
        lspExecutor.execute({
          id: `local-lsp-${Date.now()}-${keyword}`,
          name: 'lsp_query',
          arguments: { action: 'workspaceSymbols', query: keyword },
        }).then(result => ({ keyword, result }))
      )
    );

    const symbolEntries: string[] = [];
    let totalLength = 0;

    for (const entry of settled) {
      if (entry.status !== 'fulfilled') continue;
      if (totalLength >= maxLength) break;
      const { keyword, result } = entry.value;
      if (result.isError || !result.content) continue;

      try {
        const parsed = JSON.parse(result.content);
        const symbols = parsed.symbols || [];
        if (symbols.length === 0) continue;

        const formatted = symbols.slice(0, 10).map((sym: any) => {
          const loc = sym.location;
          const uri = loc?.uri || '';
          const rawPath = uri.replace(/^file:\/\//, '');
          const filePath = this.toWorkspaceDisplayPath(rawPath);
          const line = loc?.range?.start?.line ?? '?';
          return `  - ${sym.kindName || 'symbol'} **${sym.name}** → ${filePath}:${line}`;
        }).join('\n');

        const entryText = `### "${keyword}" 的符号定义\n${formatted}`;
        if (totalLength + entryText.length <= maxLength) {
          symbolEntries.push(entryText);
          totalLength += entryText.length;
        }
      } catch {
        logger.debug('本地搜索.LSP结果解析失败', { keyword }, LogCategory.TOOLS);
      }
    }

    return symbolEntries.length > 0 ? symbolEntries.join('\n\n') : null;
  }

  // ============================================================================
  // 缓存管理
  // ============================================================================

  private normalizeCacheKey(query: string): string {
    const normalized = query.trim().toLowerCase().replace(/\s+/g, ' ');
    return crypto.createHash('md5').update(normalized).digest('hex');
  }

  private cacheSet(key: string, result: string | null): void {
    // LRU 淘汰
    if (this.cache.has(key)) this.cache.delete(key);
    while (this.cache.size >= LocalCodeSearchService.CACHE_MAX_SIZE) {
      const firstKey = this.cache.keys().next().value;
      if (firstKey !== undefined) this.cache.delete(firstKey);
    }
    this.cache.set(key, { result, timestamp: Date.now() });
  }

  private toWorkspaceDisplayPath(absolutePath: string): string {
    return this.workspaceRoots.toDisplayPath(absolutePath);
  }
}
