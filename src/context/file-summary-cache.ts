/**
 * 文件摘要缓存模块
 *
 * 实现文件摘要的缓存机制，减少 Worker 重复读取同一文件
 * 缓存 key 格式: `${filePath}::${fileHash}`
 *
 * 设计参考：docs/context/unified-memory-plan.md 5.2 节和 8.1 节
 */

// ============================================================================
// 类型定义
// ============================================================================

/**
 * 来源标识
 * 标识摘要由哪个组件/Worker 产生
 */
export type ContextSource = 'orchestrator' | 'claude' | 'codex' | 'gemini';

/**
 * 结构化文件摘要
 * 摘要长度控制在 200-500 tokens
 */
export interface FileSummary {
  /** 文件目的/职责 - 一句话描述 */
  purpose: string;

  /** 核心逻辑概述 - 2-3 句话概述主要逻辑流程 */
  coreLogic: string;

  /** 关键接口/导出 - 主要的类、函数、接口（最多 5 个） */
  keyExports?: string[];

  /** 依赖关系 - 主要依赖（最多 3 个） */
  dependencies?: string[];

  /** 代码行数 */
  lineCount: number;

  /** 是否包含敏感逻辑 */
  hasSensitiveLogic?: boolean;
}

/**
 * 文件摘要缓存条目
 * 每个条目绑定文件路径和内容 hash，hash 变更即失效
 */
export interface FileSummaryCacheEntry {
  /** 文件路径 */
  filePath: string;

  /** 文件内容 hash（变更即失效） */
  fileHash: string;

  /** 结构化摘要（200-500 tokens） */
  summary: FileSummary;

  /** 产生者 - 标识由哪个 Worker 生成 */
  source: ContextSource;

  /** 更新时间（毫秒时间戳） */
  updatedAt: number;
}

// ============================================================================
// FileSummaryCache 类实现
// ============================================================================

/**
 * 文件摘要缓存
 *
 * 核心功能：
 * 1. 缓存文件摘要，避免多个 Worker 重复读取同一文件
 * 2. 基于文件 hash 的自动失效机制
 * 3. 来源追踪，每条摘要带有 source 和 updatedAt
 *
 * 缓存 key 格式: `${filePath}::${fileHash}`
 */
export class FileSummaryCache {
  /** 内部缓存存储 */
  private cache: Map<string, FileSummaryCacheEntry> = new Map();

  // --------------------------------------------------------------------------
  // 私有方法
  // --------------------------------------------------------------------------

  /**
   * 生成缓存 key
   * 格式: `${filePath}::${fileHash}`
   *
   * @param filePath - 文件路径
   * @param fileHash - 文件内容 hash
   * @returns 缓存 key
   */
  private getKey(filePath: string, fileHash: string): string {
    return `${filePath}::${fileHash}`;
  }

  // --------------------------------------------------------------------------
  // 公共方法
  // --------------------------------------------------------------------------

  /**
   * 获取摘要（hash 必须匹配）
   *
   * 只有当文件路径和 hash 都匹配时才返回摘要
   * 如果 hash 不匹配（文件已变更），返回 null
   *
   * @param filePath - 文件路径
   * @param currentHash - 当前文件的 hash
   * @returns 文件摘要，未命中或 hash 不匹配时返回 null
   */
  get(filePath: string, currentHash: string): FileSummary | null {
    const key = this.getKey(filePath, currentHash);
    const entry = this.cache.get(key);

    // 检查缓存是否存在且 hash 匹配
    if (entry && entry.fileHash === currentHash) {
      return entry.summary;
    }

    // Hash 不匹配或缓存不存在，旧摘要自动失效
    return null;
  }

  /**
   * 写入摘要
   *
   * 写入前会自动清理同一文件的旧 hash 摘要
   * 确保每个文件只保留最新 hash 对应的摘要
   *
   * @param filePath - 文件路径
   * @param fileHash - 文件内容 hash
   * @param summary - 结构化摘要
   * @param source - 产生者标识
   */
  set(
    filePath: string,
    fileHash: string,
    summary: FileSummary,
    source: ContextSource
  ): void {
    const key = this.getKey(filePath, fileHash);

    // 先清理同一文件的旧 hash 摘要
    this.invalidateOldHashes(filePath, fileHash);

    // 写入新摘要
    this.cache.set(key, {
      filePath,
      fileHash,
      summary,
      source,
      updatedAt: Date.now(),
    });
  }

  /**
   * 检查是否存在有效摘要
   *
   * @param filePath - 文件路径
   * @param fileHash - 文件内容 hash
   * @returns 是否存在匹配的有效摘要
   */
  has(filePath: string, fileHash: string): boolean {
    const key = this.getKey(filePath, fileHash);
    return this.cache.has(key);
  }

  /**
   * 使同一文件的旧 hash 摘要失效
   *
   * 遍历缓存，删除所有同一文件但 hash 不匹配的条目
   * 确保文件变更后旧摘要不会被继续使用
   *
   * @param filePath - 文件路径
   * @param newHash - 新的文件 hash
   */
  invalidateOldHashes(filePath: string, newHash: string): void {
    for (const [key, entry] of this.cache.entries()) {
      // 同一文件但 hash 不同，删除旧缓存
      if (entry.filePath === filePath && entry.fileHash !== newHash) {
        this.cache.delete(key);
      }
    }
  }

  /**
   * 获取缓存条目（包含完整元数据）
   *
   * 用于需要获取来源和更新时间等元数据的场景
   *
   * @param filePath - 文件路径
   * @param fileHash - 文件内容 hash
   * @returns 完整的缓存条目，未命中时返回 null
   */
  getEntry(filePath: string, fileHash: string): FileSummaryCacheEntry | null {
    const key = this.getKey(filePath, fileHash);
    const entry = this.cache.get(key);

    if (entry && entry.fileHash === fileHash) {
      return entry;
    }

    return null;
  }

  /**
   * 获取缓存统计信息
   *
   * @returns 缓存统计数据
   */
  getStats(): { entryCount: number; filePaths: string[] } {
    const filePaths = new Set<string>();
    for (const entry of this.cache.values()) {
      filePaths.add(entry.filePath);
    }

    return {
      entryCount: this.cache.size,
      filePaths: Array.from(filePaths),
    };
  }

  /**
   * 清空缓存
   *
   * 用于 Mission 结束时清理或测试场景
   */
  clear(): void {
    this.cache.clear();
  }

  /**
   * 删除指定文件的所有摘要
   *
   * 删除某个文件的所有缓存条目（无论 hash）
   *
   * @param filePath - 文件路径
   * @returns 删除的条目数量
   */
  deleteByFilePath(filePath: string): number {
    let deletedCount = 0;

    for (const [key, entry] of this.cache.entries()) {
      if (entry.filePath === filePath) {
        this.cache.delete(key);
        deletedCount++;
      }
    }

    return deletedCount;
  }
}
