/**
 * SearchCache — LRU 搜索缓存
 *
 * 缓存最近的搜索结果，避免对相同/相似查询重复执行完整搜索流水线。
 *
 * 特性：
 * - LRU 淘汰策略（最近最少使用）
 * - TTL 过期（默认 60 秒）
 * - 文件变更时全量失效
 * - 查询归一化（去除多余空格、统一小写）
 */

import * as crypto from 'crypto';

// ============================================================================
// 类型定义
// ============================================================================

/** 缓存条目 */
interface CacheEntry<T> {
  /** 缓存的搜索结果 */
  value: T;
  /** 写入时间戳 */
  timestamp: number;
  /** 原始查询（用于调试） */
  query: string;
}

/** 缓存配置 */
export interface SearchCacheConfig {
  /** 最大缓存条目数（默认 20） */
  maxSize: number;
  /** 条目过期时间（毫秒，默认 60000） */
  ttlMs: number;
}

const DEFAULT_CONFIG: SearchCacheConfig = {
  maxSize: 20,
  ttlMs: 60_000,
};

// ============================================================================
// SearchCache 类
// ============================================================================

export class SearchCache<T = any> {
  private cache = new Map<string, CacheEntry<T>>();
  private config: SearchCacheConfig;
  private hitCount = 0;
  private missCount = 0;

  constructor(config?: Partial<SearchCacheConfig>) {
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 查询缓存
   * @returns 命中则返回缓存结果，否则返回 undefined
   */
  get(query: string): T | undefined {
    const key = this.normalizeKey(query);
    const entry = this.cache.get(key);

    if (!entry) {
      this.missCount++;
      return undefined;
    }

    // 检查 TTL
    if (Date.now() - entry.timestamp > this.config.ttlMs) {
      this.cache.delete(key);
      this.missCount++;
      return undefined;
    }

    // LRU：删除并重新插入（移到末尾）
    this.cache.delete(key);
    this.cache.set(key, entry);
    this.hitCount++;
    return entry.value;
  }

  /**
   * 写入缓存
   */
  set(query: string, value: T): void {
    const key = this.normalizeKey(query);

    // 如果已存在，先删除（刷新位置）
    if (this.cache.has(key)) {
      this.cache.delete(key);
    }

    // LRU 淘汰：超过容量时删除最早的条目
    while (this.cache.size >= this.config.maxSize) {
      const firstKey = this.cache.keys().next().value;
      if (firstKey !== undefined) {
        this.cache.delete(firstKey);
      }
    }

    this.cache.set(key, {
      value,
      timestamp: Date.now(),
      query,
    });
  }

  /**
   * 全量失效（文件变更时调用）
   */
  invalidateAll(): void {
    this.cache.clear();
  }

  /**
   * 获取缓存统计
   */
  getStats(): {
    size: number;
    maxSize: number;
    hitCount: number;
    missCount: number;
    hitRate: string;
  } {
    const total = this.hitCount + this.missCount;
    return {
      size: this.cache.size,
      maxSize: this.config.maxSize,
      hitCount: this.hitCount,
      missCount: this.missCount,
      hitRate: total > 0 ? `${((this.hitCount / total) * 100).toFixed(1)}%` : '0%',
    };
  }

  /**
   * 查询归一化 → 缓存 key
   * 去除多余空格、统一小写、生成 hash
   */
  private normalizeKey(query: string): string {
    const normalized = query.trim().toLowerCase().replace(/\s+/g, ' ');
    return crypto.createHash('md5').update(normalized).digest('hex');
  }
}

