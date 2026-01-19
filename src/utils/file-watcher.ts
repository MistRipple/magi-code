import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../logging';

export interface FileWatcherOptions {
  root: string;
  ignore?: string[];
}

/**
 * 文件监控器（用于捕获实际文件变更）
 * 使用 fs.watch 递归监听，适配 macOS
 */
export class FileWatcher {
  private watcher: fs.FSWatcher | null = null;
  private changedFiles = new Set<string>();
  private baselineContent = new Map<string, string>();
  private root: string;
  private ignore: string[];

  constructor(options: FileWatcherOptions) {
    this.root = options.root;
    this.ignore = options.ignore ?? [];
  }

  start(): void {
    if (this.watcher) {
      throw new Error('FileWatcher already started');
    }
    if (!this.root || !fs.existsSync(this.root)) {
      throw new Error(`FileWatcher root not found: ${this.root}`);
    }

    // 🔧 采集基线内容，用于后续生成快照
    this.collectBaseline(this.root);

    this.watcher = fs.watch(this.root, { recursive: true }, (_eventType, filename) => {
      if (!filename) return;
      const absolute = path.resolve(this.root, filename);
      if (this.shouldIgnore(absolute)) return;
      this.changedFiles.add(absolute);
    });

    logger.info('子代理.文件_监听.开始', { root: this.root }, LogCategory.WORKER);
  }

  stop(): void {
    if (this.watcher) {
      this.watcher.close();
      this.watcher = null;
      logger.info('子代理.文件_监听.停止', undefined, LogCategory.WORKER);
    }
  }

  getChangedFiles(): string[] {
    return Array.from(this.changedFiles);
  }

  getBaselineContent(filePath: string): string | null {
    return this.baselineContent.get(filePath) ?? null;
  }

  reset(): void {
    this.changedFiles.clear();
    this.baselineContent.clear();
  }

  private collectBaseline(dirPath: string): void {
    if (this.shouldIgnore(dirPath)) return;
    let entries: fs.Dirent[] = [];
    try {
      entries = fs.readdirSync(dirPath, { withFileTypes: true });
    } catch (error) {
      logger.warn('子代理.文件_监听.读取_目录_失败', { path: dirPath, error }, LogCategory.WORKER);
      return;
    }

    for (const entry of entries) {
      const absolute = path.join(dirPath, entry.name);
      if (this.shouldIgnore(absolute)) continue;
      if (entry.isDirectory()) {
        this.collectBaseline(absolute);
      } else if (entry.isFile()) {
        try {
          const buffer = fs.readFileSync(absolute);
          if (!this.isTextBuffer(buffer)) {
            logger.warn('子代理.文件_监听.基线.跳过_二进制', { path: absolute }, LogCategory.WORKER);
            continue;
          }
          const content = buffer.toString('utf-8');
          this.baselineContent.set(absolute, content);
        } catch (error) {
          logger.warn('子代理.文件_监听.基线.读取_失败', { path: absolute, error }, LogCategory.WORKER);
        }
      }
    }
  }

  private shouldIgnore(filePath: string): boolean {
    const normalized = filePath.replace(/\\/g, '/');
    const rootNormalized = this.root.replace(/\\/g, '/');
    if (!normalized.startsWith(rootNormalized)) {
      return true;
    }
    return this.ignore.some(pattern => normalized.includes(pattern));
  }

  private isTextBuffer(buffer: Buffer): boolean {
    const sampleSize = Math.min(buffer.length, 1024);
    let controlChars = 0;
    for (let i = 0; i < sampleSize; i += 1) {
      const byte = buffer[i];
      if (byte === 9 || byte === 10 || byte === 13) {
        continue;
      }
      if (byte < 32 || byte === 127) {
        controlChars += 1;
      }
    }
    return controlChars / sampleSize < 0.05;
  }
}
