/**
 * CodebaseRetrievalService — 代码库检索基础设施
 *
 * 统一封装本地三级检索：
 * - L1: PKB.search()（TF-IDF + 符号索引 + 依赖图）
 * - L2: Grep 精确匹配
 * - L3: LSP workspace 符号搜索
 *
 * 特性：
 * - L1/L2/L3 并行执行
 * - LRU 结果缓存，降低重复查询开销
 * - 支持可选 scope_paths，限制检索范围
 */

import * as crypto from 'crypto';
import { logger, LogCategory } from '../logging';
import type { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';
import { WorkspaceFolderInfo, WorkspaceRoots } from '../workspace/workspace-roots';

export interface CodebaseRetrievalDeps {
  getKnowledgeBase: () => ProjectKnowledgeBase | undefined;
  executeTool: (toolCall: {
    id: string;
    name: string;
    arguments: Record<string, any>;
  }) => Promise<{ content: string; isError?: boolean }>;
  extractKeywords: (query: string) => string[];
  workspaceFolders: WorkspaceFolderInfo[];
}

interface CacheEntry {
  result: string | null;
  timestamp: number;
}

interface PkbEntry {
  filePath: string;
  snippetText: string;
  baseScore: number;
  combinedScore: number;
  lspBoost: number;
  scopeBoost: number;
}

interface PkbSearchResult {
  entries: PkbEntry[];
  coveredFiles: Set<string>;
}

interface LspSearchResult {
  text: string;
  fileScores: Map<string, number>;
}

export class CodebaseRetrievalService {
  private static readonly CACHE_MAX_SIZE = 30;
  private static readonly CACHE_TTL_MS = 45_000;
  private static readonly LSP_KIND_WEIGHTS: Record<string, number> = {
    Class: 1.0,
    Interface: 0.95,
    Function: 0.90,
    Method: 0.86,
    Constructor: 0.85,
    TypeParameter: 0.82,
    Enum: 0.80,
    Variable: 0.70,
    Constant: 0.72,
    Field: 0.68,
    Property: 0.68,
    Module: 0.66,
    Namespace: 0.66,
  };

  private cache = new Map<string, CacheEntry>();
  private workspaceRoots: WorkspaceRoots;

  constructor(private deps: CodebaseRetrievalDeps) {
    this.workspaceRoots = new WorkspaceRoots(deps.workspaceFolders);
  }

  get isAvailable(): boolean {
    const kb = this.deps.getKnowledgeBase();
    const kbReady = !!(kb?.getSearchEngine?.()?.isReady);
    return kbReady || typeof this.deps.executeTool === 'function';
  }

  invalidateCache(): void {
    this.cache.clear();
  }

  async search(query: string, maxResults: number = 10, scopePaths: string[] = []): Promise<string | null> {
    const normalizedScopePaths = this.normalizeScopePaths(scopePaths);
    const cacheKey = this.normalizeCacheKey(query, normalizedScopePaths);
    const cached = this.cache.get(cacheKey);
    if (cached && (Date.now() - cached.timestamp) < CodebaseRetrievalService.CACHE_TTL_MS) {
      this.cache.delete(cacheKey);
      this.cache.set(cacheKey, cached);
      return cached.result;
    }

    const startTime = Date.now();
    const maxContextLength = 6000;
    const keywords = this.deps.extractKeywords(query);

    const [l1Result, l2Result, l3Result] = await Promise.allSettled([
      this.pkbSearch(query, maxResults, maxContextLength, normalizedScopePaths),
      this.grepSearch(keywords, maxContextLength, normalizedScopePaths),
      this.lspSearch(keywords, maxContextLength, normalizedScopePaths),
    ]);

    const parts: string[] = [];
    let currentLength = 0;

    // L1 已覆盖的文件路径（用于 L2/L3 去重）
    let l1CoveredFiles = new Set<string>();

    const l1 = l1Result.status === 'fulfilled' ? l1Result.value : null;
    const l3 = l3Result.status === 'fulfilled' ? l3Result.value : null;
    if (l1) {
      const boostedEntries = this.mergeLspAndScopeBoost(
        l1.entries,
        l3?.fileScores || new Map<string, number>(),
        normalizedScopePaths
      );
      const l1Text = this.formatPkbEntries(boostedEntries);
      if (l1Text) {
        parts.push(l1Text);
        currentLength += l1Text.length;
        l1CoveredFiles = new Set(boostedEntries.map(entry => entry.filePath));
      }
    }

    const l2 = l2Result.status === 'fulfilled' ? l2Result.value : null;
    if (l2 && currentLength < maxContextLength * 0.8) {
      // 去重：过滤掉 L1 已覆盖文件的 grep 结果段落
      const dedupedL2 = this.deduplicateByFile(l2, l1CoveredFiles);
      if (dedupedL2) {
        const budget = maxContextLength - currentLength;
        const trimmed = dedupedL2.length > budget ? dedupedL2.substring(0, budget) + '\n... (更多结果已省略)' : dedupedL2;
        parts.push(`## 关键词匹配\n${trimmed}`);
        currentLength += trimmed.length;
      }
    }

    if (l3 && currentLength < maxContextLength * 0.9) {
      // 去重：过滤掉 L1 已覆盖文件的 LSP 结果段落
      const dedupedL3 = this.deduplicateByFile(l3.text, l1CoveredFiles);
      if (dedupedL3) {
        const budget = maxContextLength - currentLength;
        const trimmed = dedupedL3.length > budget ? dedupedL3.substring(0, budget) : dedupedL3;
        parts.push(`## 符号定义\n${trimmed}`);
        currentLength += trimmed.length;
      }
    }

    const scopeInfo = normalizedScopePaths.length > 0
      ? `\nScope: ${normalizedScopePaths.join(', ')}`
      : '';
    const result = parts.length === 0
      ? null
      : `Query: "${query}"${scopeInfo}\nSearched via local codebase retrieval (PKB + Grep + LSP)\n\n${parts.join('\n\n')}`;

    this.cacheSet(cacheKey, result);

    const elapsed = Date.now() - startTime;
    logger.debug('代码检索.完成', {
      query: query.substring(0, 80),
      scopePaths: normalizedScopePaths,
      elapsed: `${elapsed}ms`,
      hasL1: !!l1,
      hasL2: !!l2,
      hasL3: !!l3,
      lspScoredFiles: l3?.fileScores.size || 0,
      totalLength: currentLength,
    }, LogCategory.TOOLS);

    return result;
  }

  private async pkbSearch(
    query: string,
    maxResults: number,
    maxContextLength: number,
    scopePaths: string[]
  ): Promise<PkbSearchResult | null> {
    const kb = this.deps.getKnowledgeBase();
    if (!kb) return null;

    try {
      const results = await kb.search(query, {
        maxResults,
        maxContextLength: Math.floor(maxContextLength * 0.6),
        enableLLMExpansion: true,
        preferredScopes: scopePaths,
        preferRecentEdits: true,
      });
      if (results.length === 0) return null;

      const entries: PkbEntry[] = results.map((result) => {
        const snippetText = result.snippets
          .map(snippet => '```\n' + snippet.content + '\n```')
          .join('\n');
        return {
          filePath: result.filePath,
          snippetText,
          baseScore: result.score,
          combinedScore: result.score,
          lspBoost: 0,
          scopeBoost: 0,
        };
      });
      const coveredFiles = new Set(results.map(r => r.filePath));
      return { entries, coveredFiles };
    } catch (error) {
      logger.warn('代码检索.PKB搜索失败', { error }, LogCategory.TOOLS);
      return null;
    }
  }

  private async grepSearch(
    keywords: string[],
    maxLength: number,
    scopePaths: string[]
  ): Promise<string | null> {
    const searchKeywords = keywords.filter(kw => kw.length >= 3).slice(0, 3);
    if (searchKeywords.length === 0) return null;

    const effectiveScopes = scopePaths.length > 0 ? scopePaths.slice(0, 3) : [undefined];
    const jobs = effectiveScopes.flatMap(scopePath =>
      searchKeywords.map(keyword => ({ keyword, scopePath }))
    );
    if (jobs.length === 0) return null;

    const maxPerJob = Math.floor(maxLength / jobs.length);
    const settled = await Promise.allSettled(
      jobs.map(({ keyword, scopePath }) => {
        const escapedKeyword = keyword.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        return this.deps.executeTool({
          id: `retrieval-grep-${Date.now()}-${keyword}`,
          name: 'code_search_regex',
          arguments: {
            pattern: escapedKeyword,
            path: scopePath,
            include: '*.ts,*.tsx,*.js,*.jsx,*.py,*.go,*.rs,*.java,*.c,*.cpp,*.h,*.hpp,*.cs,*.php,*.rb,*.swift,*.kt,*.m,*.vue',
            context_lines: 2,
            case_sensitive: false,
          },
        }).then(result => ({ keyword, scopePath, result }));
      })
    );

    const parts: string[] = [];
    for (const entry of settled) {
      if (entry.status !== 'fulfilled') continue;
      const { keyword, scopePath, result } = entry.value;
      if (result.isError || !result.content || result.content === 'No matches found') continue;
      const truncated = result.content.length > maxPerJob
        ? result.content.substring(0, maxPerJob) + '\n... (更多结果已省略)'
        : result.content;
      const scopeLabel = scopePath ? ` @ ${scopePath}` : '';
      parts.push(`### 关键词: "${keyword}"${scopeLabel}\n${truncated}`);
    }
    return parts.length > 0 ? parts.join('\n\n') : null;
  }

  private async lspSearch(
    keywords: string[],
    maxLength: number,
    scopePaths: string[]
  ): Promise<LspSearchResult | null> {
    const symbolKeywords = keywords
      .filter(kw => /^[a-zA-Z_][a-zA-Z0-9_]*$/.test(kw) && kw.length >= 3)
      .slice(0, 3);
    if (symbolKeywords.length === 0) return null;

    const settled = await Promise.allSettled(
      symbolKeywords.map(keyword =>
        this.deps.executeTool({
          id: `retrieval-lsp-${Date.now()}-${keyword}`,
          name: 'code_intel_query',
          arguments: { action: 'workspaceSymbols', query: keyword },
        }).then(result => ({ keyword, result }))
      )
    );

    const symbolEntries: string[] = [];
    let totalLength = 0;
    const rawFileScores = new Map<string, number>();

    for (const entry of settled) {
      if (entry.status !== 'fulfilled') continue;
      if (totalLength >= maxLength) break;
      const { keyword, result } = entry.value;
      if (result.isError || !result.content) continue;

      try {
        const parsed = JSON.parse(result.content);
        const symbols = Array.isArray(parsed.symbols) ? parsed.symbols : [];
        if (symbols.length === 0) continue;

        const formatted = symbols
          .slice(0, 10)
          .map((sym: any) => {
            const loc = sym.location;
            const uri = typeof loc?.uri === 'string' ? loc.uri : '';
            const rawPath = uri.replace(/^file:\/\//, '');
            const filePath = this.toWorkspaceDisplayPath(rawPath);
            if (scopePaths.length > 0 && !this.matchesScope(filePath, scopePaths)) {
              return null;
            }
            const line = loc?.range?.start?.line ?? '?';
            const symbolScore = this.scoreLspSymbol(sym, keyword);
            rawFileScores.set(filePath, (rawFileScores.get(filePath) || 0) + symbolScore);
            return `  - ${sym.kindName || sym.kind || 'symbol'} **${sym.name}** → ${filePath}:${line} (lsp: ${symbolScore.toFixed(2)})`;
          })
          .filter((line: string | null): line is string => Boolean(line))
          .join('\n');

        if (!formatted) {
          continue;
        }

        const entryText = `### "${keyword}" 的符号定义\n${formatted}`;
        if (totalLength + entryText.length <= maxLength) {
          symbolEntries.push(entryText);
          totalLength += entryText.length;
        }
      } catch {
        logger.debug('代码检索.LSP结果解析失败', { keyword }, LogCategory.TOOLS);
      }
    }

    if (symbolEntries.length === 0) return null;
    return {
      text: symbolEntries.join('\n\n'),
      fileScores: this.normalizeFileScores(rawFileScores),
    };
  }

  private scoreLspSymbol(symbol: any, keyword: string): number {
    const kindName = String(symbol?.kind || symbol?.kindName || '');
    const normalizedKind = kindName && kindName !== 'symbol' ? kindName : String(symbol?.kindName || 'Unknown');
    const kindWeight = CodebaseRetrievalService.LSP_KIND_WEIGHTS[normalizedKind] ?? 0.6;
    const name = String(symbol?.name || '').toLowerCase();
    const kw = keyword.toLowerCase();

    let nameScore = 0;
    if (name === kw) nameScore = 0.35;
    else if (name.startsWith(kw)) nameScore = 0.25;
    else if (name.includes(kw)) nameScore = 0.15;

    return Math.min(1.3, kindWeight + nameScore);
  }

  private normalizeFileScores(rawScores: Map<string, number>): Map<string, number> {
    if (rawScores.size === 0) return rawScores;
    const maxScore = Math.max(...Array.from(rawScores.values()));
    if (maxScore <= 0) return rawScores;
    const normalized = new Map<string, number>();
    for (const [filePath, score] of rawScores.entries()) {
      normalized.set(filePath, Math.min(1, score / maxScore));
    }
    return normalized;
  }

  private mergeLspAndScopeBoost(
    entries: PkbEntry[],
    lspFileScores: Map<string, number>,
    scopePaths: string[]
  ): PkbEntry[] {
    const boosted = entries.map((entry) => {
      const lspScore = lspFileScores.get(entry.filePath) || 0;
      const scopeScore = scopePaths.length > 0 && this.matchesScope(entry.filePath, scopePaths) ? 1 : 0;
      const lspBoost = lspScore * 0.20;
      const scopeBoost = scopeScore * 0.15;
      const combinedScore = entry.baseScore * (1 + lspBoost + scopeBoost);
      return {
        ...entry,
        lspBoost,
        scopeBoost,
        combinedScore,
      };
    });
    boosted.sort((a, b) => b.combinedScore - a.combinedScore);
    return boosted;
  }

  private formatPkbEntries(entries: PkbEntry[]): string {
    if (entries.length === 0) return '';
    return entries
      .map((entry) => {
        const reasonParts: string[] = [];
        if (entry.lspBoost > 0) reasonParts.push(`lsp:+${entry.lspBoost.toFixed(2)}`);
        if (entry.scopeBoost > 0) reasonParts.push(`scope:+${entry.scopeBoost.toFixed(2)}`);
        const reason = reasonParts.length > 0 ? `; ${reasonParts.join(', ')}` : '';
        return `### ${entry.filePath} (score: ${entry.combinedScore.toFixed(2)}${reason})\n${entry.snippetText}`;
      })
      .join('\n\n');
  }

  private normalizeCacheKey(query: string, scopePaths: string[]): string {
    const normalized = query.trim().toLowerCase().replace(/\s+/g, ' ');
    const scopeSegment = scopePaths.map(scope => scope.toLowerCase()).join('|');
    return crypto.createHash('md5').update(`${normalized}::${scopeSegment}`).digest('hex');
  }

  private cacheSet(key: string, result: string | null): void {
    if (this.cache.has(key)) this.cache.delete(key);
    while (this.cache.size >= CodebaseRetrievalService.CACHE_MAX_SIZE) {
      const firstKey = this.cache.keys().next().value;
      if (firstKey !== undefined) this.cache.delete(firstKey);
    }
    this.cache.set(key, { result, timestamp: Date.now() });
  }

  /**
   * 去重：按段落分割文本，移除引用了已覆盖文件的段落
   * L2/L3 结果中的文件路径出现在段落标题行（### 关键词: / ### "xxx" 的符号定义）
   */
  private deduplicateByFile(text: string, coveredFiles: Set<string>): string | null {
    if (coveredFiles.size === 0) return text;

    const paragraphs = text.split(/\n(?=###\s)/);
    const filtered = paragraphs.filter(paragraph => {
      // 检查段落中是否包含已覆盖文件的路径
      for (const filePath of coveredFiles) {
        if (paragraph.includes(filePath)) return false;
      }
      return true;
    });

    return filtered.length > 0 ? filtered.join('\n') : null;
  }

  private normalizeScopePaths(scopePaths: string[]): string[] {
    if (!Array.isArray(scopePaths) || scopePaths.length === 0) {
      return [];
    }
    return scopePaths
      .filter(scope => typeof scope === 'string')
      .map(scope => scope.trim())
      .filter(Boolean)
      .slice(0, 5)
      .map(scope => scope.replace(/\\/g, '/').replace(/^\.\/+/, ''));
  }

  private matchesScope(filePath: string, scopePaths: string[]): boolean {
    if (scopePaths.length === 0) return true;
    const normalizedFilePath = filePath.replace(/\\/g, '/');
    return scopePaths.some(scope =>
      normalizedFilePath === scope
      || normalizedFilePath.startsWith(`${scope}/`)
      || normalizedFilePath.includes(`/${scope}/`)
      || normalizedFilePath.endsWith(`/${scope}`)
    );
  }

  private toWorkspaceDisplayPath(absolutePath: string): string {
    return this.workspaceRoots.toDisplayPath(absolutePath);
  }
}
