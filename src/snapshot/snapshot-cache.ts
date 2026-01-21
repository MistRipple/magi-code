/**
 * Snapshot Cache - 快照缓存模块
 *
 * 职责：
 * - 文件内容缓存
 * - 快照内容缓存
 * - LRU 缓存管理
 * - 缓存失效
 */

import * as fs from 'fs';
import { logger, LogCategory } from '../logging';

export class SnapshotCache {
  private fileContentCache: Map<string, string> = new Map();
  private snapshotContentCache: Map<string, string> = new Map();
  private readonly maxCacheSize: number;

  constructor(maxCacheSize: number = 100) {
    this.maxCacheSize = maxCacheSize;
  }

  /**
   * 读取文件内容（带缓存）
   */
  readFileWithCache(filePath: string): string {
    // 检查缓存
    if (this.fileContentCache.has(filePath)) {
      return this.fileContentCache.get(filePath)!;
    }

    // 读取文件
    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      this.addToCache(this.fileContentCache, filePath, content);
      return content;
    } catch (error) {
      logger.error('快照.读取文件失败', { filePath, error }, LogCategory.RECOVERY);
      throw error;
    }
  }

  /**
   * 读取快照内容（带缓存）
   */
  readSnapshotWithCache(snapshotFilePath: string): string {
    // 检查缓存
    if (this.snapshotContentCache.has(snapshotFilePath)) {
      return this.snapshotContentCache.get(snapshotFilePath)!;
    }

    // 读取快照
    try {
      const content = fs.readFileSync(snapshotFilePath, 'utf-8');
      this.addToCache(this.snapshotContentCache, snapshotFilePath, content);
      return content;
    } catch (error) {
      logger.error('快照.读取快照失败', { snapshotFilePath, error }, LogCategory.RECOVERY);
      throw error;
    }
  }

  /**
   * 添加到缓存（LRU 策略）
   */
  addToCache(cache: Map<string, string>, key: string, value: string): void {
    // 如果已存在，先删除（保证插入到末尾）
    if (cache.has(key)) {
      cache.delete(key);
    }

    // 添加新条目
    cache.set(key, value);

    // 如果超过最大缓存大小，删除最旧的条目（Map 的第一个元素）
    if (cache.size > this.maxCacheSize) {
      const firstKey = cache.keys().next().value as string;
      cache.delete(firstKey);
    }
  }

  /**
   * 使文件缓存失效
   */
  invalidateFileCache(filePath: string): void {
    this.fileContentCache.delete(filePath);
  }

  /**
   * 使快照缓存失效
   */
  invalidateSnapshotCache(snapshotFilePath: string): void {
    this.snapshotContentCache.delete(snapshotFilePath);
  }

  /**
   * 清空所有缓存
   */
  clearAll(): void {
    this.fileContentCache.clear();
    this.snapshotContentCache.clear();
  }

  /**
   * 获取缓存统计
   */
  getStats(): {
    fileCache: { size: number; maxSize: number };
    snapshotCache: { size: number; maxSize: number };
  } {
    return {
      fileCache: {
        size: this.fileContentCache.size,
        maxSize: this.maxCacheSize,
      },
      snapshotCache: {
        size: this.snapshotContentCache.size,
        maxSize: this.maxCacheSize,
      },
    };
  }
}
