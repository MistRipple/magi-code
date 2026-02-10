/**
 * ACE 索引管理器 - 管理文件收集、索引和搜索操作
 * 基于 ace-tool 的实现，遵循其索引格式
 */

import { logger, LogCategory } from '../logging';
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import ignore from 'ignore';

type IgnoreInstance = ReturnType<typeof ignore>;

/** Blob 接口 */
export interface Blob {
  path: string;
  content: string;
}

/** 索引结果接口 */
export interface IndexResult {
  status: 'success' | 'partial_success' | 'error';
  message: string;
  stats?: {
    total_blobs: number;
    existing_blobs: number;
    new_blobs: number;
  };
}

/** 搜索结果接口 */
export interface SearchResult {
  status: 'success' | 'error';
  content: string;
  stats?: {
    total_blobs: number;
    query: string;
  };
}

/** Blob 信息（用于返回给调用者） */
export interface BlobInfo {
  hash: string;
  path: string;
  content: string;
}

/** 单个 blob 的最大字节数（500KB） */
const MAX_BLOB_SIZE = 500 * 1024;

/** 默认支持的文本文件扩展名 */
const DEFAULT_TEXT_EXTENSIONS = new Set([
  '.py', '.js', '.ts', '.jsx', '.tsx', '.mjs', '.cjs',
  '.java', '.go', '.rs', '.cpp', '.c', '.cc', '.h', '.hpp', '.cs', '.rb', '.php',
  '.swift', '.kt', '.scala', '.lua', '.dart', '.pl', '.r', '.R', '.jl',
  '.md', '.mdx', '.txt', '.json', '.yaml', '.yml', '.toml', '.xml', '.ini', '.conf',
  '.html', '.htm', '.css', '.scss', '.sass', '.less', '.vue', '.svelte',
  '.sql', '.sh', '.bash', '.zsh', '.ps1', '.bat', '.cmd',
  '.graphql', '.proto', '.prisma'
]);

/** 默认排除模式 */
const DEFAULT_EXCLUDE_PATTERNS = [
  '.venv', 'venv', 'node_modules', 'vendor', '.pnpm', '.yarn',
  '.git', '.svn', '.hg', '__pycache__', '.pytest_cache', '.mypy_cache',
  'dist', 'build', 'target', 'out', 'bin', 'obj',
  '.next', '.nuxt', '.cache', '.temp', '.tmp', 'coverage', '.nyc_output',
  '.idea', '.vscode', '.vs', '.DS_Store', 'Thumbs.db',
  '*.pyc', '*.so', '*.dll', '*.exe', '*.o', '*.class',
  '*.min.js', '*.min.css', '*.bundle.js', '*.map',
  '*.gz', '*.zip', '*.tar', '*.rar',
  'package-lock.json', 'yarn.lock', 'pnpm-lock.yaml',
  '*.log', 'logs', 'tmp', 'temp',
  '*.png', '*.jpg', '*.jpeg', '*.gif', '*.ico', '*.svg',
  '*.mp3', '*.mp4', '*.pdf', '*.doc', '*.xls',
  '*.woff', '*.woff2', '*.ttf', '*.eot',
  '*.db', '*.sqlite', '*.sqlite3',
  '.ace-tool', '.magi'
];

/** 计算 blob 名称（SHA-256 哈希） */
function calculateBlobName(filePath: string, content: string): string {
  const hash = crypto.createHash('sha256');
  hash.update(filePath, 'utf-8');
  hash.update(content, 'utf-8');
  return hash.digest('hex');
}

/** 清理文件内容 */
function sanitizeContent(content: string): string {
  return content.replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/g, '');
}

/** 检查是否为二进制内容 */
function isBinaryContent(content: string): boolean {
  const nonPrintable = content.match(/[\x00-\x08\x0E-\x1F\x7F]/g) || [];
  return nonPrintable.length > content.length * 0.1;
}

/** 获取索引文件路径（遵循 ace-tool 目录结构） */
function getIndexFilePath(projectRoot: string): string {
  return path.join(projectRoot, '.ace-tool', 'index.json');
}

/** 根据文件块数量获取自适应上传策略 */
function getUploadStrategy(blobCount: number) {
  if (blobCount < 100) return { batchSize: 10, concurrency: 1, timeout: 30000 };
  if (blobCount < 500) return { batchSize: 30, concurrency: 2, timeout: 45000 };
  if (blobCount < 2000) return { batchSize: 50, concurrency: 3, timeout: 60000 };
  return { batchSize: 70, concurrency: 4, timeout: 90000 };
}

/** 延迟函数 */
function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

/**
 * ACE 索引管理器类
 */
export class AceIndexManager {
  private projectRoot: string;
  private baseUrl: string;
  private token: string;
  private textExtensions: Set<string>;
  private maxLinesPerBlob: number;
  private excludePatterns: string[];
  private indexFilePath: string;

  constructor(projectRoot: string, baseUrl: string, token: string, maxLinesPerBlob: number = 800) {
    this.projectRoot = projectRoot;
    this.baseUrl = baseUrl.replace(/\/$/, '');
    this.token = token;
    this.textExtensions = DEFAULT_TEXT_EXTENSIONS;
    this.maxLinesPerBlob = maxLinesPerBlob;
    this.excludePatterns = DEFAULT_EXCLUDE_PATTERNS;
    this.indexFilePath = getIndexFilePath(projectRoot);
  }

  /** 加载指定目录的 .gitignore 文件 */
  private loadGitignoreFromDir(dirPath: string): IgnoreInstance | null {
    const gitignorePath = path.join(dirPath, '.gitignore');
    if (!fs.existsSync(gitignorePath)) return null;
    try {
      const content = fs.readFileSync(gitignorePath, 'utf-8');
      return ignore().add(content.split('\n'));
    } catch {
      return null;
    }
  }

  /** 加载根目录 .gitignore 文件 */
  private loadGitignore(): IgnoreInstance | null {
    return this.loadGitignoreFromDir(this.projectRoot);
  }

  /** 检查路径是否应该被排除（支持多层 .gitignore） */
  private shouldExcludeWithSpec(
    _filePath: string,
    relativePath: string,
    gitignoreSpec: IgnoreInstance | null,
    isDir: boolean
  ): boolean {
    try {
      // 检查 gitignore 规则
      if (gitignoreSpec) {
        if (gitignoreSpec.ignores(isDir ? relativePath + '/' : relativePath)) return true;
      }
      // 检查内置排除模式
      const pathParts = relativePath.split('/');
      for (const pattern of this.excludePatterns) {
        for (const part of pathParts) {
          if (this.matchPattern(part, pattern)) return true;
        }
      }
      return false;
    } catch {
      return false;
    }
  }

  /** 简单的模式匹配 */
  private matchPattern(str: string, pattern: string): boolean {
    const regexPattern = pattern.replace(/\*/g, '.*').replace(/\?/g, '.');
    return new RegExp(`^${regexPattern}$`).test(str);
  }

  /** 加载索引数据 */
  loadIndex(): string[] {
    if (!fs.existsSync(this.indexFilePath)) return [];
    try {
      return JSON.parse(fs.readFileSync(this.indexFilePath, 'utf-8'));
    } catch {
      return [];
    }
  }

  /** 保存索引数据 */
  private saveIndex(blobNames: string[]): void {
    const dir = path.dirname(this.indexFilePath);
    if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(this.indexFilePath, JSON.stringify(blobNames, null, 2), 'utf-8');
  }

  /** 将文件内容分割为多个 blob */
  private splitFileContent(filePath: string, content: string): Blob[] {
    const lines = content.split(/\r?\n/);
    if (lines.length <= this.maxLinesPerBlob) return [{ path: filePath, content }];
    const blobs: Blob[] = [];
    const numChunks = Math.ceil(lines.length / this.maxLinesPerBlob);
    for (let i = 0; i < numChunks; i++) {
      const start = i * this.maxLinesPerBlob;
      const end = Math.min(start + this.maxLinesPerBlob, lines.length);
      blobs.push({ path: `${filePath}#chunk${i + 1}of${numChunks}`, content: lines.slice(start, end).join('\n') });
    }
    return blobs;
  }

  /** 收集所有文本文件（支持嵌套 .gitignore） */
  private async collectFiles(): Promise<Blob[]> {
    const blobs: Blob[] = [];
    // 加载根目录 .gitignore
    const rootGitignore = this.loadGitignore();

    // 递归遍历目录，支持每个目录的 .gitignore
    const walkDir = async (dirPath: string, parentIgnore: IgnoreInstance | null): Promise<void> => {
      // 合并当前目录的 .gitignore
      let currentIgnore = parentIgnore;
      const localGitignore = this.loadGitignoreFromDir(dirPath);
      if (localGitignore) {
        // 合并父级和当前目录的 gitignore 规则
        currentIgnore = ignore();
        if (parentIgnore) {
          // 注意：ignore 库不支持直接合并，这里重新创建
          currentIgnore = parentIgnore;
        }
        // 将当前目录的规则添加到忽略实例
        currentIgnore = localGitignore;
      }

      const entries = await fs.promises.readdir(dirPath, { withFileTypes: true });
      for (const entry of entries) {
        const fullPath = path.join(dirPath, entry.name);
        const relativePath = path.relative(this.projectRoot, fullPath).replace(/\\/g, '/');

        if (entry.isDirectory()) {
          // 检查目录是否应该被排除
          if (!this.shouldExcludeWithSpec(fullPath, relativePath, currentIgnore, true)) {
            await walkDir(fullPath, currentIgnore);
          }
        } else if (entry.isFile()) {
          // 检查文件是否应该被排除
          if (this.shouldExcludeWithSpec(fullPath, relativePath, currentIgnore, false)) continue;
          const ext = path.extname(entry.name).toLowerCase();
          if (!this.textExtensions.has(ext)) continue;
          try {
            if (relativePath.startsWith('..')) continue;
            const content = await fs.promises.readFile(fullPath, 'utf-8');
            if (isBinaryContent(content)) continue;
            const cleanContent = sanitizeContent(content);
            if (Buffer.byteLength(cleanContent, 'utf-8') > MAX_BLOB_SIZE) continue;
            blobs.push(...this.splitFileContent(relativePath, cleanContent));
          } catch { /* 忽略读取失败 */ }
        }
      }
    };

    await walkDir(this.projectRoot, rootGitignore);
    return blobs;
  }

  /** 上传 blob 批次到服务器 */
  private async uploadBatch(blobs: Blob[], timeout: number): Promise<string[]> {
    const response = await fetch(`${this.baseUrl}/batch-upload`, {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${this.token}`,
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({ blobs }),
      signal: AbortSignal.timeout(timeout)
    });
    if (!response.ok) {
      if (response.status === 401) throw new Error('Token 无效或已过期');
      if (response.status === 403) throw new Error('访问被拒绝');
      throw new Error(`上传失败: ${response.status}`);
    }
    const data = await response.json() as { blob_names?: string[] };
    return data.blob_names || [];
  }

  /** 对项目进行索引（支持增量索引） */
  async indexProject(): Promise<IndexResult> {
    logger.info('ACE.索引.开始', { projectRoot: this.projectRoot }, LogCategory.SYSTEM);
    try {
      const blobs = await this.collectFiles();
      if (blobs.length === 0) {
        return { status: 'error', message: '未找到可索引的文本文件' };
      }
      logger.info('ACE.索引.扫描.完成', { blobCount: blobs.length }, LogCategory.SYSTEM);

      // 计算当前所有 blob 的哈希
      const currentBlobHashes: string[] = [];
      const blobHashMap = new Map<string, Blob>();
      for (const blob of blobs) {
        const hash = calculateBlobName(blob.path, blob.content);
        currentBlobHashes.push(hash);
        blobHashMap.set(hash, blob);
      }

      // 加载已索引的 blob 哈希
      const existingBlobNames = new Set(this.loadIndex());

      // 找出需要上传的新 blob
      const newHashes = currentBlobHashes.filter(h => !existingBlobNames.has(h));
      const blobsToUpload = newHashes.map(h => blobHashMap.get(h)!);

      logger.info(
        'ACE.索引.增量.摘要',
        { total: currentBlobHashes.length, existing: existingBlobNames.size, new: newHashes.length },
        LogCategory.SYSTEM
      );

      if (blobsToUpload.length === 0) {
        // 无需上传，但仍需更新索引（可能有文件被删除）
        this.saveIndex(currentBlobHashes);
        logger.info('ACE.索引.上传.跳过', { total: currentBlobHashes.length }, LogCategory.SYSTEM);
        return {
          status: 'success',
          message: `索引完成，共 ${currentBlobHashes.length} 个文件块`,
          stats: { total_blobs: currentBlobHashes.length, existing_blobs: currentBlobHashes.length, new_blobs: 0 }
        };
      }

      // 上传新 blob
      const strategy = getUploadStrategy(blobsToUpload.length);
      let uploadedCount = 0;
      for (let i = 0; i < blobsToUpload.length; i += strategy.batchSize) {
        const batch = blobsToUpload.slice(i, i + strategy.batchSize);
        try {
          await this.uploadBatch(batch, strategy.timeout);
          uploadedCount += batch.length;
          logger.info('ACE.索引.上传.进度', { uploaded: uploadedCount, total: blobsToUpload.length }, LogCategory.SYSTEM);
        } catch (error) {
          logger.error('ACE.索引.上传.批次_失败', error, LogCategory.SYSTEM);
        }
      }

      // 保存当前所有 blob 的哈希作为索引
      this.saveIndex(currentBlobHashes);

      return {
        status: 'success',
        message: `索引完成，共 ${currentBlobHashes.length} 个文件块`,
        stats: { total_blobs: currentBlobHashes.length, existing_blobs: existingBlobNames.size, new_blobs: uploadedCount }
      };
    } catch (error) {
      logger.error('ACE.索引.失败', error, LogCategory.SYSTEM);
      const message = error instanceof Error ? error.message : String(error);
      return { status: 'error', message };
    }
  }

  /**
   * 收集当前所有 blob 并计算哈希
   * 用于搜索时传递给服务器
   */
  async collectBlobsWithHashes(): Promise<{ blobs: Blob[]; hashes: string[]; hashMap: Map<string, Blob> }> {
    const blobs = await this.collectFiles();
    const hashes: string[] = [];
    const hashMap = new Map<string, Blob>();

    for (const blob of blobs) {
      const hash = calculateBlobName(blob.path, blob.content);
      hashes.push(hash);
      hashMap.set(hash, blob);
    }

    return { blobs, hashes, hashMap };
  }

  /**
   * 增量索引并返回当前所有 blob 哈希
   * 用于搜索前确保索引是最新的
   */
  async ensureIndexedAndGetHashes(): Promise<{ hashes: string[]; indexResult: IndexResult }> {
    const { blobs, hashes, hashMap } = await this.collectBlobsWithHashes();

    if (blobs.length === 0) {
      return {
        hashes: [],
        indexResult: { status: 'error', message: '未找到可索引的文本文件' }
      };
    }

    // 加载已索引的 blob 哈希
    const existingBlobNames = new Set(this.loadIndex());

    // 找出需要上传的新 blob
    const newHashes = hashes.filter(h => !existingBlobNames.has(h));
    const blobsToUpload = newHashes.map(h => hashMap.get(h)!);

    if (blobsToUpload.length > 0) {
      // 上传新 blob
      const strategy = getUploadStrategy(blobsToUpload.length);
      let uploadedCount = 0;

      for (let i = 0; i < blobsToUpload.length; i += strategy.batchSize) {
        const batch = blobsToUpload.slice(i, i + strategy.batchSize);
        try {
          await this.uploadBatch(batch, strategy.timeout);
          uploadedCount += batch.length;
        } catch (error) {
          logger.error('ACE.索引.上传.批次_失败', error, LogCategory.SYSTEM);
        }
      }

      // 保存当前所有 blob 的哈希作为索引
      this.saveIndex(hashes);

      return {
        hashes,
        indexResult: {
          status: 'success',
          message: `索引完成，共 ${hashes.length} 个文件块`,
          stats: { total_blobs: hashes.length, existing_blobs: existingBlobNames.size, new_blobs: uploadedCount }
        }
      };
    }

    // 无需上传，但仍需更新索引
    this.saveIndex(hashes);

    return {
      hashes,
      indexResult: {
        status: 'success',
        message: `索引完成，共 ${hashes.length} 个文件块`,
        stats: { total_blobs: hashes.length, existing_blobs: hashes.length, new_blobs: 0 }
      }
    };
  }

  /**
   * 执行语义搜索
   * @param query 自然语言查询
   * @param ensureIndexed 是否先确保索引是最新的
   */
  async search(query: string, ensureIndexed: boolean = true): Promise<SearchResult> {
    logger.info('ACE.搜索.开始', { query, ensureIndexed }, LogCategory.SYSTEM);

    try {
      // 获取当前所有 blob 哈希
      let hashes: string[];

      if (ensureIndexed) {
        const result = await this.ensureIndexedAndGetHashes();
        hashes = result.hashes;
        if (hashes.length === 0) {
          return { status: 'error', content: '未找到可索引的文本文件' };
        }
      } else {
        // 直接使用本地索引
        hashes = this.loadIndex();
        if (hashes.length === 0) {
          // 没有索引，需要先索引
          const result = await this.ensureIndexedAndGetHashes();
          hashes = result.hashes;
          if (hashes.length === 0) {
            return { status: 'error', content: '未找到可索引的文本文件' };
          }
        }
      }

      // 调用搜索 API - 使用 Augment 风格的 blobs 对象格式
      const response = await fetch(`${this.baseUrl}/agents/codebase-retrieval`, {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${this.token}`,
          'Content-Type': 'application/json'
        },
        body: JSON.stringify({
          information_request: query,
          blobs: {
            checkpoint_id: null,
            added_blobs: hashes,
            deleted_blobs: []
          }
        }),
        signal: AbortSignal.timeout(60000) // 搜索超时 60 秒
      });

      if (!response.ok) {
        if (response.status === 401) {
          return { status: 'error', content: 'Token 无效或已过期' };
        }
        if (response.status === 403) {
          return { status: 'error', content: '访问被拒绝' };
        }
        return { status: 'error', content: `搜索失败: HTTP ${response.status}` };
      }

      const data = await response.json() as { formatted_retrieval?: string };
      const content = data.formatted_retrieval || '未找到相关代码';

      logger.info('ACE.搜索.完成', { query, resultLength: content.length }, LogCategory.SYSTEM);

      return {
        status: 'success',
        content,
        stats: { total_blobs: hashes.length, query }
      };
    } catch (error) {
      logger.error('ACE.搜索.失败', error, LogCategory.SYSTEM);
      const message = error instanceof Error ? error.message : String(error);
      return { status: 'error', content: `搜索失败: ${message}` };
    }
  }

  /** 获取项目根目录 */
  getProjectRoot(): string {
    return this.projectRoot;
  }

  /** 获取 API 配置状态 */
  isConfigured(): boolean {
    return !!(this.baseUrl && this.token);
  }
}
