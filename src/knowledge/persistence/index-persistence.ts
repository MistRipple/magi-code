/**
 * IndexPersistence — 索引持久化管理器
 *
 * 职责：
 * - 将 InvertedIndex / SymbolIndex / DependencyGraph 序列化到磁盘
 * - 启动时从磁盘加载并验证文件新鲜度
 * - 检测已删除/修改/新增文件，驱动增量同步
 * - 防抖保存（避免频繁写盘）
 */

import * as fs from 'fs';
import * as path from 'path';
import * as zlib from 'zlib';
import { logger, LogCategory } from '../../logging';
import { atomicWriteFileSync } from '../../utils/atomic-write';
import { InvertedIndex, InvertedIndexSnapshot } from '../indexing/inverted-index';
import { SymbolIndex, SymbolIndexSnapshot } from '../indexing/symbol-index';
import { DependencyGraph, DependencyGraphSnapshot } from '../indexing/dependency-graph';
import { ExpansionCacheSnapshot } from '../search/query-expander';

// ============================================================================
// 类型定义
// ============================================================================

/** 持久化文件清单条目 */
interface FileManifestEntry {
  /** 最后修改时间（毫秒时间戳） */
  mtime: number;
  /** 文件大小（字节） */
  size: number;
  /** 文件类型 */
  type: 'source' | 'config' | 'doc' | 'test';
}

/** 持久化快照顶层结构 */
interface PersistenceSnapshot {
  /** 格式版本号 */
  version: number;
  /** 项目根目录 */
  projectRoot: string;
  /** 创建时间 */
  createdAt: number;
  /** 最后更新时间 */
  updatedAt: number;
  /** 文件清单（相对路径 → 元数据） */
  fileManifest: Array<[string, FileManifestEntry]>;
  /** 倒排索引快照 */
  invertedIndex: InvertedIndexSnapshot;
  /** 符号索引快照 */
  symbolIndex: SymbolIndexSnapshot;
  /** 依赖图快照 */
  dependencyGraph: DependencyGraphSnapshot;
  /** 查询扩展缓存快照（可选，向后兼容） */
  expansionCache?: ExpansionCacheSnapshot;
}

/** 新鲜度验证结果 */
export interface FreshnessResult {
  /** 未变化的文件 */
  unchanged: string[];
  /** 已修改的文件 */
  modified: string[];
  /** 已删除的文件（索引中有，磁盘上无） */
  deleted: string[];
  /** 新增的文件（磁盘上有，索引中无） */
  added: string[];
}

/** 当前格式版本 */
const PERSISTENCE_VERSION = 1;

/** 默认防抖延迟（毫秒） */
const DEFAULT_DEBOUNCE_MS = 5000;

// ============================================================================
// IndexPersistence 类
// ============================================================================

export class IndexPersistence {
  private cacheFilePath: string;
  /** 旧版未压缩路径（用于迁移清理） */
  private legacyCacheFilePath: string;
  private debounceTimer: ReturnType<typeof setTimeout> | null = null;
  private debounceMs: number;

  constructor(projectRoot: string, debounceMs = DEFAULT_DEBOUNCE_MS) {
    this.cacheFilePath = path.join(projectRoot, '.magi', 'cache', 'search-index.json.gz');
    this.legacyCacheFilePath = path.join(projectRoot, '.magi', 'cache', 'search-index.json');
    this.debounceMs = debounceMs;
  }

  /**
   * 保存索引到磁盘
   */
  save(
    projectRoot: string,
    invertedIndex: InvertedIndex,
    symbolIndex: SymbolIndex,
    dependencyGraph: DependencyGraph,
    files: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>,
    expansionCache?: ExpansionCacheSnapshot
  ): void {
    try {
      const dir = path.dirname(this.cacheFilePath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }

      // 构建文件清单
      const fileManifest: Array<[string, FileManifestEntry]> = [];
      for (const file of files) {
        try {
          const fullPath = path.join(projectRoot, file.path);
          const stat = fs.statSync(fullPath);
          fileManifest.push([file.path, {
            mtime: stat.mtimeMs,
            size: stat.size,
            type: file.type,
          }]);
        } catch {
          // 文件可能已被删除，跳过
        }
      }

      const snapshot: PersistenceSnapshot = {
        version: PERSISTENCE_VERSION,
        projectRoot,
        createdAt: Date.now(),
        updatedAt: Date.now(),
        fileManifest,
        invertedIndex: invertedIndex.toJSON(),
        symbolIndex: symbolIndex.toJSON(),
        dependencyGraph: dependencyGraph.toJSON(),
        expansionCache,
      };

      const jsonStr = JSON.stringify(snapshot);
      const compressed = zlib.gzipSync(jsonStr);
      atomicWriteFileSync(this.cacheFilePath, compressed);

      // 清理旧版未压缩文件
      if (fs.existsSync(this.legacyCacheFilePath)) {
        try { fs.unlinkSync(this.legacyCacheFilePath); } catch { /* 忽略 */ }
      }

      logger.info('索引持久化.保存成功', {
        files: fileManifest.length,
        rawSize: `${(jsonStr.length / 1024).toFixed(0)}KB`,
        compressedSize: `${(compressed.length / 1024).toFixed(0)}KB`,
        path: this.cacheFilePath,
      }, LogCategory.SESSION);
    } catch (error) {
      logger.warn('索引持久化.保存失败', { error, path: this.cacheFilePath }, LogCategory.SESSION);
    }
  }

  /**
   * 防抖保存：延迟写盘，合并多次连续调用
   */
  debouncedSave(
    projectRoot: string,
    invertedIndex: InvertedIndex,
    symbolIndex: SymbolIndex,
    dependencyGraph: DependencyGraph,
    files: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>,
    expansionCache?: ExpansionCacheSnapshot
  ): void {
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
    }
    this.debounceTimer = setTimeout(() => {
      this.debounceTimer = null;
      this.save(projectRoot, invertedIndex, symbolIndex, dependencyGraph, files, expansionCache);
    }, this.debounceMs);
  }

  /**
   * 从磁盘加载索引快照
   * 返回 null 表示无可用缓存
   */
  load(): PersistenceSnapshot | null {
    try {
      let raw: string;

      if (fs.existsSync(this.cacheFilePath)) {
        // 优先读取压缩格式
        const compressed = fs.readFileSync(this.cacheFilePath);
        raw = zlib.gunzipSync(compressed).toString('utf-8');
      } else if (fs.existsSync(this.legacyCacheFilePath)) {
        // 向后兼容：读取旧版未压缩格式
        raw = fs.readFileSync(this.legacyCacheFilePath, 'utf-8');
      } else {
        return null;
      }

      const snapshot = JSON.parse(raw) as PersistenceSnapshot;

      // 版本校验
      if (snapshot.version !== PERSISTENCE_VERSION) {
        logger.info('索引持久化.版本不匹配，丢弃缓存', {
          cached: snapshot.version,
          current: PERSISTENCE_VERSION,
        }, LogCategory.SESSION);
        return null;
      }

      logger.info('索引持久化.加载成功', {
        files: snapshot.fileManifest.length,
        updatedAt: new Date(snapshot.updatedAt).toISOString(),
      }, LogCategory.SESSION);

      return snapshot;
    } catch (error) {
      logger.warn('索引持久化.加载失败', { error }, LogCategory.SESSION);
      return null;
    }
  }

  /**
   * 验证文件新鲜度：对比持久化清单与当前文件系统
   */
  validateFreshness(
    projectRoot: string,
    snapshot: PersistenceSnapshot,
    currentFiles: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>
  ): FreshnessResult {
    const result: FreshnessResult = {
      unchanged: [],
      modified: [],
      deleted: [],
      added: [],
    };

    const manifest = new Map(snapshot.fileManifest);
    const currentFileSet = new Set(currentFiles.map(f => f.path));

    // 检查清单中的文件：是否删除或修改
    for (const [filePath, entry] of manifest) {
      if (!currentFileSet.has(filePath)) {
        result.deleted.push(filePath);
        continue;
      }

      try {
        const fullPath = path.join(projectRoot, filePath);
        if (!fs.existsSync(fullPath)) {
          result.deleted.push(filePath);
          continue;
        }
        const stat = fs.statSync(fullPath);
        if (Math.abs(stat.mtimeMs - entry.mtime) > 1) {
          // mtime 有变化 → 文件已修改
          result.modified.push(filePath);
        } else {
          result.unchanged.push(filePath);
        }
      } catch {
        result.deleted.push(filePath);
      }
    }

    // 检查新增文件：当前存在但清单中没有
    for (const file of currentFiles) {
      if (!manifest.has(file.path)) {
        result.added.push(file.path);
      }
    }

    return result;
  }

  /**
   * 恢复索引并执行增量同步
   * 返回 { restored: true, expansionCache } 表示成功恢复，{ restored: false } 表示需要全量重建
   */
  restoreAndSync(
    projectRoot: string,
    invertedIndex: InvertedIndex,
    symbolIndex: SymbolIndex,
    dependencyGraph: DependencyGraph,
    currentFiles: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>
  ): { restored: boolean; expansionCache?: ExpansionCacheSnapshot } {
    const snapshot = this.load();
    if (!snapshot) return { restored: false };

    // 验证新鲜度
    const freshness = this.validateFreshness(projectRoot, snapshot, currentFiles);

    const changeCount = freshness.modified.length + freshness.deleted.length + freshness.added.length;

    // 如果变化文件超过总文件数的 30%，直接全量重建更高效
    const totalFiles = snapshot.fileManifest.length + freshness.added.length;
    if (totalFiles > 0 && changeCount / totalFiles > 0.3) {
      logger.info('索引持久化.变化过多，全量重建', {
        changeCount,
        totalFiles,
        ratio: `${Math.round(changeCount / totalFiles * 100)}%`,
      }, LogCategory.SESSION);
      return { restored: false };
    }

    // 从快照恢复索引
    try {
      invertedIndex.fromJSON(snapshot.invertedIndex);
      symbolIndex.fromJSON(snapshot.symbolIndex);

      // DependencyGraph 需要当前文件集合
      const fileSet = new Set(currentFiles.map(f => f.path));
      dependencyGraph.fromJSON(snapshot.dependencyGraph, projectRoot, fileSet);
    } catch (error) {
      logger.warn('索引持久化.恢复失败，需全量重建', { error }, LogCategory.SESSION);
      return { restored: false };
    }

    // 执行增量同步
    // 1. 删除已不存在的文件
    for (const filePath of freshness.deleted) {
      invertedIndex.removeFile(filePath);
      symbolIndex.removeFile(filePath);
      dependencyGraph.removeFile(filePath);
    }

    // 2. 更新已修改的文件
    for (const filePath of freshness.modified) {
      invertedIndex.updateFile(projectRoot, filePath);
      symbolIndex.updateFile(projectRoot, filePath);
      dependencyGraph.updateFile(projectRoot, filePath);
    }

    // 3. 新增文件
    for (const filePath of freshness.added) {
      invertedIndex.updateFile(projectRoot, filePath);
      symbolIndex.updateFile(projectRoot, filePath);
      dependencyGraph.updateFile(projectRoot, filePath);
    }

    logger.info('索引持久化.增量同步完成', {
      unchanged: freshness.unchanged.length,
      modified: freshness.modified.length,
      deleted: freshness.deleted.length,
      added: freshness.added.length,
    }, LogCategory.SESSION);

    return { restored: true, expansionCache: snapshot.expansionCache };
  }

  /**
   * 删除缓存文件
   */
  invalidate(): void {
    try {
      for (const filePath of [this.cacheFilePath, this.legacyCacheFilePath]) {
        if (fs.existsSync(filePath)) {
          fs.unlinkSync(filePath);
        }
      }
      logger.info('索引持久化.缓存已删除', undefined, LogCategory.SESSION);
    } catch {
      // 忽略删除失败
    }
  }

  /**
   * 取消待执行的防抖保存
   */
  dispose(): void {
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = null;
    }
  }
}

