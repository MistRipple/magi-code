/**
 * ProjectKnowledgeBase - 项目级知识库
 *
 * 提供项目结构、架构决策、常见问题等上下文信息
 *
 * 核心功能：
 * 1. 代码索引 - 扫描项目文件和目录结构
 * 2. ADR 管理 - 存储和检索架构决策记录
 * 3. FAQ 管理 - 存储和检索常见问题
 * 4. 上下文生成 - 为 LLM 生成项目上下文
 */

import * as fs from 'fs';
import * as path from 'path';
import ignore, { Ignore } from 'ignore';
import { logger, LogCategory } from '../logging';
import { LLMConfigLoader } from '../llm/config';
import { LLMClient, LLMMessageParams } from '../llm/types';
import { LocalSearchEngine, SearchOptions, SearchResult, SearchEngineConfig } from './local-search-engine';
import { estimateMaxCharsForTokens, estimateTokenCount } from '../utils/token-estimator';

// ============================================================================
// 类型定义
// ============================================================================

/**
 * 技术栈信息
 */
export interface TechStack {
  languages: string[];
  frameworks: string[];
  buildTools: string[];
  testFrameworks: string[];
}

/**
 * 依赖信息
 */
export interface DependencyInfo {
  dependencies: Record<string, string>;
  devDependencies: Record<string, string>;
}

/**
 * 文件条目
 */
export interface FileEntry {
  path: string;
  type: 'source' | 'config' | 'doc' | 'test';
  language?: string;
  size: number;
  /** 文件行数 */
  lines: number;
  exports?: string[];  // 导出的函数/类（未来实现）
}

/**
 * 目录条目
 */
export interface DirectoryEntry {
  path: string;
  fileCount: number;
  subdirCount: number;
}

/**
 * 代码索引
 */
export interface CodeIndex {
  files: FileEntry[];
  directories: DirectoryEntry[];
  techStack: TechStack;
  dependencies: DependencyInfo;
  entryPoints: string[];
  lastIndexed: number;
}

/**
 * ADR 状态
 */
export type ADRStatus = 'proposed' | 'accepted' | 'archived' | 'superseded';

/**
 * 架构决策记录
 */
export interface ADRRecord {
  id: string;
  title: string;
  date: number;
  status: ADRStatus;
  context: string;      // 决策背景
  decision: string;     // 决策内容
  consequences: string; // 影响和后果
  alternatives?: string[]; // 考虑过的替代方案
  relatedFiles?: string[]; // 相关文件
}

/**
 * FAQ 记录
 */
export interface FAQRecord {
  id: string;
  question: string;
  answer: string;
  category: string;
  tags: string[];
  relatedFiles?: string[];
  createdAt: number;
  updatedAt: number;
  useCount: number; // 使用次数
}

/**
 * 经验记录
 */
export interface LearningRecord {
  id: string;
  content: string;
  context: string;
  createdAt: number;
  tags?: string[];
}

export interface LearningAddResult {
  status: 'inserted' | 'duplicate' | 'rejected';
  record?: LearningRecord;
}

/**
 * 学习经验提取候选项（会话提取中间态）
 */
interface LearningExtractionCandidate {
  content: string;
  context?: string;
  tags?: string[];
}

/**
 * 项目知识库配置
 */
export interface ProjectKnowledgeConfig {
  projectRoot: string;
  storageDir?: string;  // 默认 .magi/knowledge
  indexPatterns?: string[];  // 要索引的文件模式
  ignorePatterns?: string[]; // 要忽略的文件模式
  searchEngineConfig?: SearchEngineConfig; // 搜索引擎配置（排序权重等）
}

// ============================================================================
// ProjectKnowledgeBase 类
// ============================================================================

export class ProjectKnowledgeBase {
  private projectRoot: string;
  private projectName: string;
  private storageDir: string;

  private codeIndex: CodeIndex | null = null;
  private adrs: ADRRecord[] = [];
  private faqs: FAQRecord[] = [];
  private learnings: LearningRecord[] = [];

  private indexPatterns: string[];
  private ignorePatterns: string[];
  /** 基于 gitignore 规范的路径忽略过滤器（合并 .gitignore + 内置规则 + 自定义规则） */
  private ignoreFilter: Ignore;
  private searchEngineConfig?: SearchEngineConfig;
  /** 从 indexPatterns 预编译得到的可索引扩展名集合（含点号，小写） */
  private indexedExtensions = new Set<string>();

  private llmClient: LLMClient | null = null;
  private localSearchEngine: LocalSearchEngine | null = null;

  // 容量限制：超出时淘汰最旧的记录
  private static readonly MAX_ADRS = 100;
  private static readonly MAX_FAQS = 200;
  private static readonly MAX_LEARNINGS = 200;
  private static readonly MIN_LEARNING_CONTENT_LENGTH = 12;
  private static readonly MAX_LEARNING_CONTENT_LENGTH = 600;
  private static readonly SESSION_LEARNING_MAX_RESULTS = 6;
  private static readonly LOW_VALUE_LEARNING_PATTERNS = [
    /^todo$/i,
    /^n\/a$/i,
    /^none$/i,
    /^无$/i,
    /^暂无$/i,
    /^待补充$/i,
    /^待确认$/i,
    /^unknown$/i,
    /^待处理$/i,
    /^继续观察$/i,
  ];

  constructor(config: ProjectKnowledgeConfig) {
    this.projectRoot = config.projectRoot;
    this.projectName = path.basename(this.projectRoot);
    this.storageDir = config.storageDir || path.join(this.projectRoot, '.magi', 'knowledge');

    // 默认索引模式（与 SymbolIndex LANG_PATTERNS 支持的语言对齐）
    this.indexPatterns = config.indexPatterns || [
      // JS/TS 系
      '**/*.ts', '**/*.tsx', '**/*.js', '**/*.jsx', '**/*.mjs', '**/*.cjs',
      // Python / Go / Java / Rust
      '**/*.py', '**/*.go', '**/*.java', '**/*.rs',
      // C / C++
      '**/*.c', '**/*.h', '**/*.cpp', '**/*.cc', '**/*.cxx', '**/*.hpp', '**/*.hh',
      // C# / PHP / Ruby / Swift / Kotlin
      '**/*.cs', '**/*.php', '**/*.rb', '**/*.swift', '**/*.kt', '**/*.kts',
      // Objective-C
      '**/*.m', '**/*.mm',
      // 前端框架
      '**/*.vue', '**/*.svelte',
      // 配置 / 文档
      '**/*.json', '**/*.md', '**/*.yml', '**/*.yaml',
    ];
    this.rebuildIndexedExtensions();

    // 默认忽略模式（保留字段供外部引用）
    this.ignorePatterns = config.ignorePatterns || [
      'node_modules/',
      'dist/',
      'out/',
      'build/',
      '.git/',
      '.vscode/',
      'coverage/',
      '*.min.js',
      '*.map',
    ];

    // 构建基于 gitignore 规范的路径忽略过滤器
    this.ignoreFilter = this.buildIgnoreFilter(config.ignorePatterns);

    this.searchEngineConfig = config.searchEngineConfig;
  }

  /**
   * 从 indexPatterns 提取并重建可索引扩展名集合
   */
  private rebuildIndexedExtensions(): void {
    this.indexedExtensions.clear();
    for (const pattern of this.indexPatterns) {
      // 仅提取形如 **/*.ts 或 *.ts 的扩展名
      const match = pattern.match(/\.([a-zA-Z0-9]+)$/);
      if (match && match[1]) {
        this.indexedExtensions.add(`.${match[1].toLowerCase()}`);
      }
    }
  }

  /**
   * 初始化知识库
   * 加载已有的索引、ADR、FAQ
   */
  async initialize(): Promise<void> {
    logger.info('项目知识库.初始化.开始', { projectRoot: this.projectRoot }, LogCategory.SESSION);

    // 确保存储目录存在
    await this.ensureStorageDir();

    // 加载已有数据
    await this.loadCodeIndex();
    await this.loadADRs();
    await this.loadFAQs();
    await this.loadLearnings();

    // 如果没有索引，执行首次索引
    if (!this.codeIndex) {
      await this.indexProject();
    }

    // 构建本地搜索引擎索引
    await this.buildSearchEngineIndex();

    logger.info('项目知识库.初始化.完成', {
      files: this.codeIndex?.files.length || 0,
      adrs: this.adrs.length,
      faqs: this.faqs.length,
      learnings: this.learnings.length,
      searchEngine: this.localSearchEngine?.getStats() || null,
    }, LogCategory.SESSION);
  }

  /**
   * 索引项目
   * 扫描文件、检测技术栈、生成索引
   */
  async indexProject(): Promise<CodeIndex> {
    logger.info('项目知识库.索引.开始', undefined, LogCategory.SESSION);

    const startTime = Date.now();

    // 1. 扫描文件
    const files = await this.scanFiles();

    // 2. 扫描目录
    const directories = await this.scanDirectories();

    // 3. 检测技术栈
    const techStack = await this.detectTechStack();

    // 4. 读取依赖信息
    const dependencies = await this.readDependencies();

    // 5. 识别入口文件
    const entryPoints = this.identifyEntryPoints(files);

    // 6. 创建索引
    this.codeIndex = {
      files,
      directories,
      techStack,
      dependencies,
      entryPoints,
      lastIndexed: Date.now()
    };

    // 7. 保存索引
    await this.saveCodeIndex();

    const duration = Date.now() - startTime;
    logger.info('项目知识库.索引.完成', {
      files: files.length,
      directories: directories.length,
      duration: `${duration}ms`
    }, LogCategory.SESSION);

    return this.codeIndex;
  }

  /**
   * 扫描文件（异步，不阻塞事件循环）
   */
  private async scanFiles(): Promise<FileEntry[]> {
    const files: FileEntry[] = [];

    const scanDirectory = async (dir: string) => {
      try {
        const entries = await fs.promises.readdir(dir, { withFileTypes: true });

        for (const entry of entries) {
          const fullPath = path.join(dir, entry.name);
          const relativePath = path.relative(this.projectRoot, fullPath);

          // 检查是否应该忽略（传入 isDirectory 以正确匹配 `xxx/` 规则）
          if (this.shouldIgnore(relativePath, entry.isDirectory())) {
            continue;
          }

          if (entry.isDirectory()) {
            await scanDirectory(fullPath);
          } else if (entry.isFile()) {
            // 检查文件扩展名是否匹配
            if (this.shouldIndex(relativePath)) {
              const stats = await fs.promises.stat(fullPath);
              // 统计文件行数：读取 Buffer 直接计数换行符，避免全量 split
              let lineCount = 0;
              try {
                const buf = await fs.promises.readFile(fullPath);
                for (let i = 0; i < buf.length; i++) {
                  if (buf[i] === 0x0A) lineCount++;
                }
                // 文件非空且末尾无换行 → 最后一行也计入
                if (buf.length > 0 && buf[buf.length - 1] !== 0x0A) lineCount++;
              } catch {
                // 读取失败（二进制/权限）→ 行数保持 0
              }
              files.push({
                path: relativePath,
                type: this.classifyFileType(relativePath),
                language: this.detectLanguage(relativePath),
                size: stats.size,
                lines: lineCount,
              });
            }
          }
        }
      } catch (error) {
        logger.warn('项目知识库.扫描目录.失败', { dir, error }, LogCategory.SESSION);
      }
    };

    await scanDirectory(this.projectRoot);
    return files;
  }

  /**
   * 检查文件是否应该被索引
   * 仅按预编译扩展名集合判断（避免重复 regex 计算）
   */
  private shouldIndex(filePath: string): boolean {
    const ext = path.extname(filePath).toLowerCase();
    if (!ext) return false;
    return this.indexedExtensions.has(ext);
  }

  /**
   * 获取可索引扩展名（不含点号），用于外部文件监听与索引规则保持一致
   */
  getIndexedExtensions(): string[] {
    return Array.from(this.indexedExtensions)
      .map(ext => ext.replace(/^\./, ''))
      .filter(Boolean)
      .sort();
  }

  /**
   * 构建基于 gitignore 规范的路径忽略过滤器
   * 优先级：.gitignore 规则 > 内置默认规则 > 外部自定义规则
   */
  private buildIgnoreFilter(customPatterns?: string[]): Ignore {
    const ig = ignore();

    // 1. 内置默认忽略：编译产物、依赖、缓存等
    ig.add([
      // 依赖
      'node_modules/',
      'bower_components/',
      // 编译/构建产物
      'dist/',
      'out/',
      'build/',
      '.next/',
      '.nuxt/',
      '.output/',
      'target/',
      '__pycache__/',
      // 版本控制
      '.git/',
      '.svn/',
      '.hg/',
      // IDE / 编辑器
      '.vscode/',
      '.idea/',
      '.history/',
      '.vscode-test/',
      // 系统文件
      '.DS_Store',
      'Thumbs.db',
      // 测试覆盖率
      'coverage/',
      '.nyc_output/',
      // 缓存 / 临时文件
      '*.min.js',
      '*.map',
      '*.log',
      '*.tmp',
      '*.bak',
      '*.cache',
      '*.vsix',
      // 以 . 开头的目录通用屏蔽（隐藏资源目录）
      '.*/',
    ]);

    // 2. 读取项目 .gitignore（追加，支持否定规则覆盖）
    const gitignorePath = path.join(this.projectRoot, '.gitignore');
    try {
      if (fs.existsSync(gitignorePath)) {
        const content = fs.readFileSync(gitignorePath, 'utf-8');
        ig.add(content);
        logger.info('项目知识库.忽略规则.已加载.gitignore', {
          path: gitignorePath,
        }, LogCategory.SESSION);
      }
    } catch (error) {
      logger.warn('项目知识库.忽略规则.读取.gitignore失败', {
        path: gitignorePath,
        error,
      }, LogCategory.SESSION);
    }

    // 3. 追加外部自定义规则
    if (customPatterns && customPatterns.length > 0) {
      // 将旧式 glob 模式（**/xxx/**）转换为 gitignore 规范格式
      const normalized = customPatterns.map(p =>
        p.replace(/^\*\*\//, '').replace(/\/\*\*$/, '')
      );
      ig.add(normalized);
    }

    return ig;
  }

  /**
   * 检查路径是否应该被忽略
   * 使用 gitignore 规范引擎进行标准匹配（支持通配符、否定规则等）
   *
   * @param isDirectory 当已知路径为目录时传 true，
   *   使 ignore 包能正确匹配 `xxx/` 形式的纯目录规则
   */
  private shouldIgnore(filePath: string, isDirectory?: boolean): boolean {
    // ignore 包要求正斜杠分隔的相对路径，不含前导 /
    const normalized = filePath.replace(/\\/g, '/').replace(/^\//, '');
    if (!normalized) return false;
    // gitignore 规范：规则 `xxx/` 仅匹配目录；
    // ignore 包需要路径带尾斜杠才能命中此类规则
    if (isDirectory && !normalized.endsWith('/')) {
      return this.ignoreFilter.ignores(normalized + '/');
    }
    return this.ignoreFilter.ignores(normalized);
  }

  /**
   * 扫描目录（异步，不阻塞事件循环）
   */
  private async scanDirectories(): Promise<DirectoryEntry[]> {
    const directories: DirectoryEntry[] = [];

    const scanDirectory = async (dir: string) => {
      try {
        const relativePath = path.relative(this.projectRoot, dir);

        // 跳过根目录和忽略的目录
        if (relativePath && !this.shouldIgnore(relativePath, true)) {
          const entries = await fs.promises.readdir(dir, { withFileTypes: true });

          let fileCount = 0;
          let subdirCount = 0;

          for (const entry of entries) {
            if (entry.isFile()) {
              fileCount++;
            } else if (entry.isDirectory()) {
              subdirCount++;
            }
          }

          directories.push({
            path: relativePath,
            fileCount,
            subdirCount
          });
        }

        // 递归扫描子目录
        const entries = await fs.promises.readdir(dir, { withFileTypes: true });
        for (const entry of entries) {
          if (entry.isDirectory()) {
            const fullPath = path.join(dir, entry.name);
            const subRelativePath = path.relative(this.projectRoot, fullPath);
            if (!this.shouldIgnore(subRelativePath, true)) {
              await scanDirectory(fullPath);
            }
          }
        }
      } catch (error) {
        logger.warn('项目知识库.扫描目录.失败', { dir, error }, LogCategory.SESSION);
      }
    };

    await scanDirectory(this.projectRoot);
    return directories;
  }

  /**
   * 检测技术栈（数据驱动，异步 IO）
   */
  private async detectTechStack(): Promise<TechStack> {
    const techStack: TechStack = {
      languages: [],
      frameworks: [],
      buildTools: [],
      testFrameworks: []
    };

    // 检测语言（通过配置文件存在性）
    const langDetectors: Array<[string, string]> = [
      ['tsconfig.json', 'TypeScript'],
      ['package.json', 'JavaScript'],
      ['pyproject.toml', 'Python'],
      ['go.mod', 'Go'],
      ['Cargo.toml', 'Rust'],
      ['pom.xml', 'Java'],
      ['build.gradle', 'Java/Kotlin'],
    ];

    for (const [file, lang] of langDetectors) {
      try {
        await fs.promises.access(path.join(this.projectRoot, file));
        techStack.languages.push(lang);
      } catch { /* 文件不存在 */ }
    }

    // 读取 package.json 检测框架和工具
    const packageJsonPath = path.join(this.projectRoot, 'package.json');
    try {
      const content = await fs.promises.readFile(packageJsonPath, 'utf-8');
      const packageJson = JSON.parse(content);
      const allDeps = {
        ...packageJson.dependencies,
        ...packageJson.devDependencies
      };

      // 数据驱动的框架/工具/测试检测表
      const frameworkMap: Record<string, string> = {
        'react': 'React', 'vue': 'Vue', '@angular/core': 'Angular',
        'express': 'Express', 'fastify': 'Fastify', 'koa': 'Koa',
        'next': 'Next.js', 'nuxt': 'Nuxt', 'svelte': 'Svelte',
        'vscode': 'VSCode Extension', 'electron': 'Electron',
        'nestjs': 'NestJS', '@nestjs/core': 'NestJS',
      };
      const buildToolMap: Record<string, string> = {
        'webpack': 'Webpack', 'vite': 'Vite', 'rollup': 'Rollup',
        'esbuild': 'esbuild', 'turbo': 'Turborepo', 'tsup': 'tsup',
        'parcel': 'Parcel', 'swc': 'SWC', '@swc/core': 'SWC',
      };
      const testMap: Record<string, string> = {
        'jest': 'Jest', 'mocha': 'Mocha', 'vitest': 'Vitest',
        '@playwright/test': 'Playwright', 'cypress': 'Cypress',
        'ava': 'AVA', 'tap': 'Tap', '@testing-library/react': 'Testing Library',
      };

      for (const [dep, name] of Object.entries(frameworkMap)) {
        if (allDeps[dep]) techStack.frameworks.push(name);
      }
      for (const [dep, name] of Object.entries(buildToolMap)) {
        if (allDeps[dep]) techStack.buildTools.push(name);
      }
      if (packageJson.scripts?.build) techStack.buildTools.push('npm scripts');
      for (const [dep, name] of Object.entries(testMap)) {
        if (allDeps[dep]) techStack.testFrameworks.push(name);
      }
    } catch {
      // package.json 不存在或解析失败
    }

    return techStack;
  }

  /**
   * 读取依赖信息（异步 IO）
   */
  private async readDependencies(): Promise<DependencyInfo> {
    const packageJsonPath = path.join(this.projectRoot, 'package.json');

    try {
      const content = await fs.promises.readFile(packageJsonPath, 'utf-8');
      const packageJson = JSON.parse(content);
      return {
        dependencies: packageJson.dependencies || {},
        devDependencies: packageJson.devDependencies || {}
      };
    } catch {
      return {
        dependencies: {},
        devDependencies: {}
      };
    }
  }

  /**
   * 识别入口文件
   */
  private identifyEntryPoints(files: FileEntry[]): string[] {
    const entryPoints: string[] = [];

    // 常见入口文件模式
    const entryPatterns = [
      'index.ts',
      'index.js',
      'main.ts',
      'main.js',
      'app.ts',
      'app.js',
      'src/index.ts',
      'src/index.js',
      'src/main.ts',
      'src/main.js',
      'src/extension.ts'  // VSCode 扩展入口
    ];

    for (const file of files) {
      if (entryPatterns.some(pattern => file.path.endsWith(pattern))) {
        entryPoints.push(file.path);
      }
    }

    return entryPoints;
  }

  /**
   * 分类文件类型
   */
  private classifyFileType(filePath: string): FileEntry['type'] {
    const fileName = path.basename(filePath);
    const dirName = path.dirname(filePath);

    // 配置文件
    const configFiles = [
      'package.json',
      'tsconfig.json',
      'webpack.config.js',
      'vite.config.ts',
      '.eslintrc',
      '.prettierrc'
    ];
    if (configFiles.some(cf => fileName === cf || fileName.startsWith(cf))) {
      return 'config';
    }

    // 文档文件
    if (fileName.endsWith('.md') || fileName === 'README') {
      return 'doc';
    }

    // 测试文件
    if (
      fileName.includes('.test.') ||
      fileName.includes('.spec.') ||
      dirName.includes('test') ||
      dirName.includes('__tests__')
    ) {
      return 'test';
    }

    // 源代码文件
    return 'source';
  }

  /**
   * 检测文件语言
   */
  private detectLanguage(filePath: string): string | undefined {
    const ext = path.extname(filePath);
    const languageMap: Record<string, string> = {
      '.ts': 'TypeScript',
      '.tsx': 'TypeScript',
      '.js': 'JavaScript',
      '.jsx': 'JavaScript',
      '.json': 'JSON',
      '.md': 'Markdown',
      '.yml': 'YAML',
      '.yaml': 'YAML'
    };
    return languageMap[ext];
  }

  // 防抖定时器：避免短时间内频繁重新索引
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
  private static readonly REFRESH_DEBOUNCE_MS = 30_000; // 30 秒防抖
  // 文件事件缓冲：合并高频 watcher 事件，降低乱序/抖动
  private pendingFileEvents = new Map<string, 'changed' | 'created' | 'deleted'>();
  private fileEventFlushTimer: ReturnType<typeof setTimeout> | null = null;
  private static readonly FILE_EVENT_FLUSH_MS = 120;

  /**
   * 延迟刷新代码索引（防抖）
   * 任务完成后调用，避免短时间多次任务完成触发多次全量扫描
   */
  refreshIndex(): void {
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
    }
    this.refreshTimer = setTimeout(async () => {
      this.refreshTimer = null;
      try {
        logger.info('项目知识库.索引.刷新开始', undefined, LogCategory.SESSION);
        await this.indexProject();
        // 复用已有搜索引擎实例，仅重建索引数据
        await this.buildSearchEngineIndex();
        logger.info('项目知识库.索引.刷新完成', {
          files: this.codeIndex?.files.length || 0,
          searchEngine: this.localSearchEngine?.getStats() || null,
        }, LogCategory.SESSION);
      } catch (error) {
        logger.error('项目知识库.索引.刷新失败', { error }, LogCategory.SESSION);
      }
    }, ProjectKnowledgeBase.REFRESH_DEBOUNCE_MS);
    logger.debug('项目知识库.索引.刷新已排队', {
      debounceMs: ProjectKnowledgeBase.REFRESH_DEBOUNCE_MS,
    }, LogCategory.SESSION);
  }

  /**
   * 获取代码索引
   */
  getCodeIndex(): CodeIndex | null {
    return this.codeIndex;
  }

  /**
   * 本地搜索入口（LocalSearchEngine 代理）
   * 通过此方法进行本地代码上下文检索
   */
  async search(query: string, options: SearchOptions = {}): Promise<SearchResult[]> {
    if (!this.localSearchEngine || !this.localSearchEngine.isReady) {
      return [];
    }
    return this.localSearchEngine.search(query, options);
  }

  /**
   * 获取本地搜索引擎实例（用于外部直接访问增量更新等）
   */
  getSearchEngine(): LocalSearchEngine | null {
    return this.localSearchEngine;
  }

  /**
   * 获取项目上下文（用于注入到 LLM）
   */
  getProjectContext(maxTokens: number = 800): string {
    if (!this.codeIndex) {
      return '';
    }

    const parts: string[] = [];

    // 项目基本信息
    parts.push(`**项目**: ${this.projectName}`);
    parts.push(`**技术栈**: ${this.codeIndex.techStack.languages.join(', ')}`);
    if (this.codeIndex.techStack.frameworks.length > 0) {
      parts.push(`**框架**: ${this.codeIndex.techStack.frameworks.join(', ')}`);
    }
    parts.push(`**文件数**: ${this.codeIndex.files.length} 个源文件`);
    parts.push('');

    // 关键架构决策（最多3个）
    if (this.adrs.length > 0) {
      parts.push('**关键架构决策**:');
      const acceptedADRs = this.adrs
        .filter(adr => adr.status === 'accepted')
        .slice(0, 3);
      acceptedADRs.forEach((adr, index) => {
        parts.push(`${index + 1}. [${adr.id}] ${adr.title}`);
      });
      parts.push('');
    }

    // 相关 FAQ（最多2个）
    if (this.faqs.length > 0) {
      parts.push('**相关 FAQ**:');
      const topFAQs = this.faqs
        .sort((a, b) => b.useCount - a.useCount)
        .slice(0, 2);
      topFAQs.forEach(faq => {
        parts.push(`Q: ${faq.question}`);
        parts.push(`A: ${faq.answer.substring(0, 100)}...`);
        parts.push('');
      });
    }

    const context = parts.join('\n');

    // 简单的 token 估算（1 token ≈ 4 字符）
    const estimatedTokens = estimateTokenCount(context);

    if (estimatedTokens > maxTokens) {
      // 截断到最大 tokens
      const maxChars = estimateMaxCharsForTokens(maxTokens);
      return context.substring(0, maxChars) + '...';
    }

    return context;
  }


  /**
   * 获取知识库索引（ADR/FAQ/Learning 标题列表）
   * 用于系统提示词轻量注入，避免全量预载
   */
  getKnowledgeIndex(maxTokens: number = 600): string {
    if (!this.codeIndex) {
      return '';
    }

    const sections: string[] = [];
    const maxItemsPerCategory = 20;

    const acceptedADRs = this.adrs.filter(adr => adr.status === 'accepted');
    if (acceptedADRs.length > 0) {
      sections.push('**ADR 索引**:');
      const visible = acceptedADRs.slice(0, maxItemsPerCategory);
      visible.forEach(adr => {
        sections.push(`- [${adr.id}] ${adr.title}`);
      });
      if (acceptedADRs.length > maxItemsPerCategory) {
        sections.push(`- ... (${acceptedADRs.length - maxItemsPerCategory} more)`);
      }
      sections.push('');
    }

    if (this.faqs.length > 0) {
      sections.push('**FAQ 索引**:');
      const visible = this.faqs.slice(0, maxItemsPerCategory);
      visible.forEach(faq => {
        sections.push(`- [${faq.id}] ${faq.question}`);
      });
      if (this.faqs.length > maxItemsPerCategory) {
        sections.push(`- ... (${this.faqs.length - maxItemsPerCategory} more)`);
      }
      sections.push('');
    }

    if (this.learnings.length > 0) {
      sections.push('**Learning 索引**:');
      const visible = this.learnings.slice(0, maxItemsPerCategory);
      visible.forEach(learning => {
        sections.push(`- [${learning.id}] ${learning.content.substring(0, 80)}`);
      });
      if (this.learnings.length > maxItemsPerCategory) {
        sections.push(`- ... (${this.learnings.length - maxItemsPerCategory} more)`);
      }
      sections.push('');
    }

    const indexContent = sections.join('\n').trim();
    if (!indexContent) {
      return '';
    }

    const estimatedTokens = estimateTokenCount(indexContent);
    if (estimatedTokens > maxTokens) {
      const maxChars = estimateMaxCharsForTokens(maxTokens);
      return indexContent.substring(0, maxChars) + '...';
    }

    return indexContent;
  }

  /**
   * 设置 LLM 客户端（用于自动知识提取 + 搜索引擎查询扩展）
   */
  setLLMClient(client: LLMClient): void {
    this.llmClient = client;

    // 同步传递给搜索引擎用于查询扩展
    if (this.localSearchEngine) {
      this.localSearchEngine.setLLMClient(client);
    }

    logger.info('项目知识库.LLM客户端.已设置', undefined, LogCategory.SESSION);
  }

  /**
   * 文件变更事件入口（由外部 FileSystemWatcher 调用）
   * 将事件转发给搜索引擎进行增量更新
   *
   * 增量过滤：非 delete 事件会检查扩展名是否在索引范围内，
   * 避免非索引文件触发无效的增量更新排队。
   * delete 事件不过滤（文件已不存在，但索引中可能有残留需要清理）。
   */
  onFileEvent(filePath: string, type: 'changed' | 'created' | 'deleted'): void {
    if (!this.localSearchEngine) return;

    // 增量过滤：非 delete 事件必须在索引范围内
    if (type !== 'deleted') {
      const relativePath = path.relative(this.projectRoot, filePath);
      if (!this.shouldIndex(relativePath) || this.shouldIgnore(relativePath)) {
        return;
      }
    }

    const normalizedPath = path.normalize(filePath);
    const prevType = this.pendingFileEvents.get(normalizedPath);
    this.pendingFileEvents.set(
      normalizedPath,
      this.mergeFileEventType(prevType, type)
    );

    if (this.fileEventFlushTimer) {
      clearTimeout(this.fileEventFlushTimer);
    }
    this.fileEventFlushTimer = setTimeout(() => {
      this.fileEventFlushTimer = null;
      this.flushPendingFileEvents();
    }, ProjectKnowledgeBase.FILE_EVENT_FLUSH_MS);
  }

  /**
   * 合并同一路径的高频事件，避免无意义重复更新
   */
  private mergeFileEventType(
    previous: 'changed' | 'created' | 'deleted' | undefined,
    next: 'changed' | 'created' | 'deleted'
  ): 'changed' | 'created' | 'deleted' {
    if (!previous) return next;
    if (next === 'deleted') return 'deleted';
    if (next === 'created') return previous === 'deleted' ? 'created' : previous;
    // next === 'changed'
    if (previous === 'created' || previous === 'deleted') return previous;
    return 'changed';
  }

  /**
   * 批量下发缓冲的文件事件
   */
  private flushPendingFileEvents(): void {
    if (!this.localSearchEngine || this.pendingFileEvents.size === 0) {
      this.pendingFileEvents.clear();
      return;
    }

    const events = Array.from(this.pendingFileEvents.entries());
    this.pendingFileEvents.clear();

    for (const [filePath, type] of events) {
      switch (type) {
        case 'changed':
          this.localSearchEngine.onFileChanged(filePath);
          break;
        case 'created':
          this.localSearchEngine.onFileCreated(filePath);
          break;
        case 'deleted':
          this.localSearchEngine.onFileDeleted(filePath);
          break;
      }
    }
  }

  /**
   * 从会话消息中提取 ADR
   * 使用辅助模型进行智能提取
   */
  async extractADRFromSession(messages: Array<{ role: string; content: string }>): Promise<ADRRecord[]> {
    if (!this.llmClient) {
      logger.warn('项目知识库.ADR提取.未设置LLM客户端', undefined, LogCategory.SESSION);
      return [];
    }

    try {
      // 构建提取提示词
      const prompt = this.buildADRExtractionPrompt(messages);

      // 调用 LLM 进行提取
      const response = await this.llmClient.sendMessage({
        messages: [
          { role: 'user', content: prompt }
        ],
        maxTokens: 2000,
        temperature: 0.3
      });

      // 解析响应
      const adrs = this.parseADRsFromResponse(response.content);
      logger.info('项目知识库.ADR提取.完成', { count: adrs.length }, LogCategory.SESSION);

      return adrs;
    } catch (error) {
      logger.error('项目知识库.ADR提取.失败', { error }, LogCategory.SESSION);
      return [];
    }
  }

  /**
   * 从会话消息中提取 FAQ
   * 使用辅助模型进行智能提取
   */
  async extractFAQFromSession(messages: Array<{ role: string; content: string }>): Promise<FAQRecord[]> {
    if (!this.llmClient) {
      logger.warn('项目知识库.FAQ提取.未设置LLM客户端', undefined, LogCategory.SESSION);
      return [];
    }

    try {
      // 构建提取提示词
      const prompt = this.buildFAQExtractionPrompt(messages);

      // 调用 LLM 进行提取
      const response = await this.llmClient.sendMessage({
        messages: [
          { role: 'user', content: prompt }
        ],
        maxTokens: 2000,
        temperature: 0.3
      });

      // 解析响应
      const faqs = this.parseFAQsFromResponse(response.content);
      logger.info('项目知识库.FAQ提取.完成', { count: faqs.length }, LogCategory.SESSION);

      return faqs;
    } catch (error) {
      logger.error('项目知识库.FAQ提取.失败', { error }, LogCategory.SESSION);
      return [];
    }
  }

  /**
   * 从会话消息中提取 Learning 候选（优先 LLM，失败或未配置时回退到启发式规则）
   */
  async extractLearningsFromSession(
    messages: Array<{ role: string; content: string }>
  ): Promise<LearningExtractionCandidate[]> {
    if (!Array.isArray(messages) || messages.length === 0) {
      return [];
    }

    if (this.llmClient) {
      try {
        const prompt = this.buildLearningExtractionPrompt(messages);
        const response = await this.llmClient.sendMessage({
          messages: [{ role: 'user', content: prompt }],
          maxTokens: 1600,
          temperature: 0.2,
        });
        const llmCandidates = this.parseLearningsFromResponse(response.content);
        const normalized = this.normalizeLearningCandidates(llmCandidates);
        if (normalized.length > 0) {
          logger.info('项目知识库.Learning提取.LLM完成', { count: normalized.length }, LogCategory.SESSION);
          return normalized.slice(0, ProjectKnowledgeBase.SESSION_LEARNING_MAX_RESULTS);
        }
      } catch (error) {
        logger.warn('项目知识库.Learning提取.LLM失败_回退启发式', {
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.SESSION);
      }
    }

    const heuristicCandidates = this.extractLearningsHeuristically(messages);
    const normalized = this.normalizeLearningCandidates(heuristicCandidates);
    if (normalized.length > 0) {
      logger.info('项目知识库.Learning提取.启发式完成', { count: normalized.length }, LogCategory.SESSION);
    }
    return normalized.slice(0, ProjectKnowledgeBase.SESSION_LEARNING_MAX_RESULTS);
  }

  /**
   * 构建 ADR 提取提示词
   */
  private buildADRExtractionPrompt(messages: Array<{ role: string; content: string }>): string {
    const conversationText = messages
      .map(m => `[${m.role}]: ${m.content}`)
      .join('\n\n');

    return `请从以下对话中提取架构决策记录（ADR）。

## 对话内容
${conversationText}

## 提取规则
1. 识别关键技术决策（包含关键词：决定、选择、采用、使用、方案、架构等）
2. 提取决策的背景、内容、影响
3. 识别考虑过的替代方案
4. 每个决策生成一个 ADR

## 质量过滤（必须严格遵守）
- 跳过基于错误前提或假设的讨论
- 跳过被明确否定、废弃或推翻的方案
- 跳过临时性的调试尝试（如"先试试…"）
- 不要提取与之前已有决策语义重复的内容
- 只提取最终确认采纳的决策，不提取中间讨论过程

## 输出格式
请以 JSON 数组格式输出，每个 ADR 包含以下字段：
\`\`\`json
[
  {
    "title": "决策标题",
    "context": "决策背景和原因",
    "decision": "具体决策内容",
    "consequences": "决策的影响和后果",
    "alternatives": ["替代方案1", "替代方案2"]
  }
]
\`\`\`

如果没有找到明确的架构决策，返回空数组 []。`;
  }

  /**
   * 构建 FAQ 提取提示词
   */
  private buildFAQExtractionPrompt(messages: Array<{ role: string; content: string }>): string {
    const conversationText = messages
      .map(m => `[${m.role}]: ${m.content}`)
      .join('\n\n');

    return `请从以下对话中提取常见问题（FAQ）。

## 对话内容
${conversationText}

## 提取规则
1. 识别用户提出的问题（包含关键词：如何、怎么、为什么、问题、错误等）
2. 提取助手给出的解答
3. 问题应该具有通用性，可以帮助其他用户
4. 每个问答对生成一个 FAQ

## 质量过滤（必须严格遵守）
- 跳过基于错误前提或假设的提问
- 跳过被纠正过的错误回答
- 跳过一次性的、与项目特定临时状态绑定的问题
- 不要提取与之前已有问题语义重复的内容
- 只提取有实际参考价值的问答，不提取闲聊或确认性对话

## 输出格式
请以 JSON 数组格式输出，每个 FAQ 包含以下字段：
\`\`\`json
[
  {
    "question": "问题内容",
    "answer": "详细解答",
    "category": "问题分类（如：development, debugging, configuration）",
    "tags": ["标签1", "标签2"]
  }
]
\`\`\`

如果没有找到有价值的问答，返回空数组 []。`;
  }

  /**
   * 构建 Learning 提取提示词
   */
  private buildLearningExtractionPrompt(messages: Array<{ role: string; content: string }>): string {
    const conversationText = messages
      .map(m => `[${m.role}]: ${m.content}`)
      .join('\n\n');

    return `请从以下对话中提取“可复用经验（Learning）”。

## 对话内容
${conversationText}

## 提取目标
提取那些可以跨会话复用、可指导后续工程执行的经验，例如：
- 调试结论
- 避坑建议
- 执行顺序约束
- 工具使用准则
- 风险与前置条件

## 质量要求（必须遵守）
- 经验必须具体、可执行，避免空话
- 每条经验建议 1~2 句，内容完整
- 跳过与已有经验重复或近似重复的表述
- 跳过无意义短句（如“继续观察”“待处理”等）

## 输出格式（JSON 数组）
\`\`\`json
[
  {
    "content": "经验内容",
    "context": "来源上下文（可选）",
    "tags": ["debug", "workflow"]
  }
]
\`\`\`

若无有效经验请返回 []。`;
  }

  /**
   * 从 LLM 响应文本中健壮地提取 JSON 数组
   * 多层降级：code fence → 裸 JSON 数组 → 整体尝试解析
   */
  private extractJsonArray(response: string, label: string): any[] | null {
    // 尝试 1：```json ... ``` code fence
    const fenceMatch = response.match(/```(?:json)?\s*([\s\S]*?)\s*```/);
    if (fenceMatch) {
      try {
        const parsed = JSON.parse(fenceMatch[1]);
        if (Array.isArray(parsed)) return parsed;
      } catch { /* 继续尝试 */ }
    }

    // 尝试 2：贪婪匹配最外层 [ ... ]
    const bracketMatch = response.match(/\[[\s\S]*\]/);
    if (bracketMatch) {
      try {
        const parsed = JSON.parse(bracketMatch[0]);
        if (Array.isArray(parsed)) return parsed;
      } catch { /* 继续尝试 */ }
    }

    // 尝试 3：整体 trim 后直接解析
    try {
      const parsed = JSON.parse(response.trim());
      if (Array.isArray(parsed)) return parsed;
    } catch { /* 放弃 */ }

    logger.warn(`项目知识库.${label}解析.未找到有效JSON数组`, undefined, LogCategory.SESSION);
    return null;
  }

  /**
   * 从 LLM 响应中解析 ADR
   */
  private parseADRsFromResponse(response: string): ADRRecord[] {
    try {
      const extractedADRs = this.extractJsonArray(response, 'ADR');
      if (!extractedADRs) return [];

      // 转换为 ADRRecord 格式
      // 自动提取的 ADR 直接设为 accepted，已通过提取 prompt 的质量过滤
      return extractedADRs.map((adr, index) => ({
        id: `adr-${Date.now()}-${index}`,
        title: adr.title || '未命名决策',
        date: Date.now(),
        status: 'accepted' as ADRStatus,
        context: adr.context || '',
        decision: adr.decision || '',
        consequences: adr.consequences || '',
        alternatives: adr.alternatives || []
      }));
    } catch (error) {
      logger.error('项目知识库.ADR解析.失败', { error }, LogCategory.SESSION);
      return [];
    }
  }

  /**
   * 从 LLM 响应中解析 FAQ
   */
  private parseFAQsFromResponse(response: string): FAQRecord[] {
    try {
      const extractedFAQs = this.extractJsonArray(response, 'FAQ');
      if (!extractedFAQs) return [];

      // 转换为 FAQRecord 格式
      return extractedFAQs.map((faq, index) => ({
        id: `faq-${Date.now()}-${index}`,
        question: faq.question || '未命名问题',
        answer: faq.answer || '',
        category: faq.category || 'general',
        tags: faq.tags || [],
        createdAt: Date.now(),
        updatedAt: Date.now(),
        useCount: 0
      }));
    } catch (error) {
      logger.error('项目知识库.FAQ解析.失败', { error }, LogCategory.SESSION);
      return [];
    }
  }

  /**
   * 从 LLM 响应中解析 Learning 候选
   */
  private parseLearningsFromResponse(response: string): LearningExtractionCandidate[] {
    try {
      const extracted = this.extractJsonArray(response, 'Learning');
      if (!extracted) return [];
      return extracted
        .filter((item) => item && typeof item === 'object')
        .map((item) => ({
          content: typeof item.content === 'string' ? item.content : '',
          context: typeof item.context === 'string' ? item.context : '',
          tags: Array.isArray(item.tags)
            ? item.tags.filter((tag: unknown) => typeof tag === 'string')
            : undefined,
        }));
    } catch (error) {
      logger.error('项目知识库.Learning解析.失败', { error }, LogCategory.SESSION);
      return [];
    }
  }

  /**
   * 启发式 Learning 提取（无模型时兜底）
   */
  private extractLearningsHeuristically(
    messages: Array<{ role: string; content: string }>
  ): LearningExtractionCandidate[] {
    const candidates: LearningExtractionCandidate[] = [];
    const seen = new Set<string>();
    const patterns = [
      /(?:经验|教训|结论|注意|建议|最佳实践|踩坑|坑点|要点)[：:]\s*([^\n。！？!?]{6,220})/gi,
      /(?:important|note|lesson|tip|best practice)[：:]\s*([^\n.?!]{6,220})/gi,
    ];

    for (const message of messages) {
      if (!message || message.role === 'user') continue;
      const content = typeof message.content === 'string' ? message.content : '';
      if (!content) continue;

      for (const pattern of patterns) {
        pattern.lastIndex = 0;
        let match: RegExpExecArray | null;
        while ((match = pattern.exec(content)) !== null) {
          const extracted = match[1]?.trim();
          const normalized = this.normalizeLearningTextForDedup(extracted);
          if (!normalized || seen.has(normalized)) continue;
          seen.add(normalized);
          candidates.push({
            content: extracted,
            context: `session:${message.role}`,
          });
        }
      }
    }

    // 兜底：如果没有命中关键字，尝试从最后一条 assistant 消息末段提取 1 条可执行句
    if (candidates.length === 0) {
      const lastAssistant = [...messages].reverse().find((msg) => msg?.role !== 'user' && typeof msg.content === 'string' && msg.content.trim().length > 0);
      if (lastAssistant) {
        const sentences = lastAssistant.content
          .split(/[\n。！？!?]+/)
          .map((line) => line.trim())
          .filter(Boolean)
          .filter((line) => line.length >= ProjectKnowledgeBase.MIN_LEARNING_CONTENT_LENGTH && line.length <= 220);
        const fallback = sentences.find((line) => /(?:应|需要|必须|建议|避免|确保|先|后)/.test(line));
        if (fallback) {
          candidates.push({
            content: fallback,
            context: `session:${lastAssistant.role}`,
          });
        }
      }
    }

    return candidates;
  }

  /**
   * Learning 候选标准化（裁剪、去空、质量过滤）
   */
  private normalizeLearningCandidates(candidates: LearningExtractionCandidate[]): LearningExtractionCandidate[] {
    if (!Array.isArray(candidates) || candidates.length === 0) {
      return [];
    }

    const normalized: LearningExtractionCandidate[] = [];
    for (const candidate of candidates) {
      const content = this.sanitizeLearningContent(candidate.content || '');
      const context = this.sanitizeLearningContext(candidate.context || '');
      if (!this.isLearningContentQualified(content)) {
        continue;
      }
      const duplicate = normalized.find((record) => this.isLearningDuplicate(content, context, record.content, record.context || ''));
      if (duplicate) {
        continue;
      }
      normalized.push({
        content,
        context,
        tags: this.normalizeLearningTags(candidate.tags),
      });
    }

    return normalized;
  }

  // ============================================================================
  // ADR 管理
  // ============================================================================

  /**
   * 添加 ADR（自动去重：标题相似度 > 80% 时跳过）
   */
  addADR(adr: ADRRecord): void {
    const duplicate = this.adrs.find(
      existing => this.textSimilarity(existing.title, adr.title) > 0.8,
    );
    if (duplicate) {
      logger.info('项目知识库.ADR.去重跳过', {
        existingId: duplicate.id,
        existingTitle: duplicate.title,
        newTitle: adr.title,
      }, LogCategory.SESSION);
      return;
    }
    this.adrs.push(adr);
    // 超出容量限制时淘汰最旧的记录
    if (this.adrs.length > ProjectKnowledgeBase.MAX_ADRS) {
      const removed = this.adrs.shift();
      logger.info('项目知识库.ADR.容量淘汰', { removedId: removed?.id }, LogCategory.SESSION);
    }
    this.saveADRs();
    logger.info('项目知识库.ADR.已添加', { id: adr.id, title: adr.title }, LogCategory.SESSION);
  }

  /**
   * 获取 ADRs
   */
  getADRs(filter?: { status?: ADRStatus }): ADRRecord[] {
    const { records, changed } = this.normalizeADRRecords(this.adrs);
    if (changed) {
      this.adrs = records;
      this.saveADRs();
      logger.warn('项目知识库.ADR.已自动清理(访问时)', { count: this.adrs.length }, LogCategory.SESSION);
    }
    if (!filter) {
      return this.adrs;
    }

    return this.adrs.filter(adr => {
      if (filter.status && adr.status !== filter.status) {
        return false;
      }
      return true;
    });
  }

  /**
   * 获取单个 ADR
   */
  getADR(id: string): ADRRecord | undefined {
    return this.adrs.find(adr => adr.id === id);
  }

  /**
   * 更新 ADR
   */
  updateADR(id: string, updates: Partial<ADRRecord>): boolean {
    const index = this.adrs.findIndex(adr => adr.id === id);
    if (index === -1) {
      return false;
    }

    this.adrs[index] = { ...this.adrs[index], ...updates };
    this.saveADRs();
    logger.info('项目知识库.ADR.已更新', { id }, LogCategory.SESSION);
    return true;
  }

  /**
   * 删除 ADR
   */
  deleteADR(id: string): boolean {
    const index = this.adrs.findIndex(adr => adr.id === id);
    if (index === -1) {
      return false;
    }

    this.adrs.splice(index, 1);
    this.saveADRs();
    logger.info('项目知识库.ADR.已删除', { id }, LogCategory.SESSION);
    return true;
  }

  // ============================================================================
  // FAQ 管理
  // ============================================================================

  /**
   * 添加 FAQ（自动去重：问题相似度 > 80% 时跳过）
   */
  addFAQ(faq: FAQRecord): void {
    const duplicate = this.faqs.find(
      existing => this.textSimilarity(existing.question, faq.question) > 0.8,
    );
    if (duplicate) {
      logger.info('项目知识库.FAQ.去重跳过', {
        existingId: duplicate.id,
        existingQ: duplicate.question,
        newQ: faq.question,
      }, LogCategory.SESSION);
      return;
    }
    this.faqs.push(faq);
    // 超出容量限制时淘汰使用次数最低的记录
    if (this.faqs.length > ProjectKnowledgeBase.MAX_FAQS) {
      // 按 useCount 升序排列，移除使用最少的
      this.faqs.sort((a, b) => a.useCount - b.useCount);
      const removed = this.faqs.shift();
      logger.info('项目知识库.FAQ.容量淘汰', { removedId: removed?.id }, LogCategory.SESSION);
    }
    this.saveFAQs();
    logger.info('项目知识库.FAQ.已添加', { id: faq.id, question: faq.question }, LogCategory.SESSION);
  }

  /**
   * 搜索 FAQs
   */
  searchFAQs(keyword: string): FAQRecord[] {
    const lowerKeyword = keyword.toLowerCase();
    const results = this.faqs.filter(faq => {
      return (
        faq.question.toLowerCase().includes(lowerKeyword) ||
        faq.answer.toLowerCase().includes(lowerKeyword) ||
        faq.tags.some(tag => tag.toLowerCase().includes(lowerKeyword))
      );
    });
    // 命中的 FAQ 计入使用次数
    for (const faq of results) {
      this.incrementFAQUseCount(faq.id);
    }
    return results;
  }

  /**
   * 获取所有 FAQs
   */
  getFAQs(filter?: { category?: string }): FAQRecord[] {
    const { records, changed } = this.normalizeFAQRecords(this.faqs);
    if (changed) {
      this.faqs = records;
      this.saveFAQs();
      logger.warn('项目知识库.FAQ.已自动清理(访问时)', { count: this.faqs.length }, LogCategory.SESSION);
    }
    if (!filter) {
      return this.faqs;
    }

    return this.faqs.filter(faq => {
      if (filter.category && faq.category !== filter.category) {
        return false;
      }
      return true;
    });
  }

  /**
   * 获取单个 FAQ
   */
  getFAQ(id: string): FAQRecord | undefined {
    return this.faqs.find(faq => faq.id === id);
  }

  /**
   * 更新 FAQ
   */
  updateFAQ(id: string, updates: Partial<FAQRecord>): boolean {
    const index = this.faqs.findIndex(faq => faq.id === id);
    if (index === -1) {
      return false;
    }

    this.faqs[index] = {
      ...this.faqs[index],
      ...updates,
      updatedAt: Date.now()
    };
    this.saveFAQs();
    logger.info('项目知识库.FAQ.已更新', { id }, LogCategory.SESSION);
    return true;
  }

  /**
   * 删除 FAQ
   */
  deleteFAQ(id: string): boolean {
    const index = this.faqs.findIndex(faq => faq.id === id);
    if (index === -1) {
      return false;
    }

    this.faqs.splice(index, 1);
    this.saveFAQs();
    logger.info('项目知识库.FAQ.已删除', { id }, LogCategory.SESSION);
    return true;
  }

  /**
   * 增加 FAQ 使用次数
   */
  incrementFAQUseCount(id: string): void {
    const faq = this.getFAQ(id);
    if (faq) {
      faq.useCount++;
      this.saveFAQs();
    }
  }

  // ============================================================================
  // Learning 管理
  // ============================================================================

  /**
   * 添加经验记录
   */
  addLearning(content: string, context: string, tags?: string[]): LearningAddResult {
    const normalizedContent = this.sanitizeLearningContent(content);
    const normalizedContext = this.sanitizeLearningContext(context);
    if (!this.isLearningContentQualified(normalizedContent)) {
      logger.info('项目知识库.Learning.质量过滤跳过', {
        contentPreview: normalizedContent.substring(0, 60),
      }, LogCategory.SESSION);
      return { status: 'rejected' };
    }

    const duplicate = this.findDuplicateLearning(normalizedContent, normalizedContext);
    if (duplicate) {
      logger.info('项目知识库.Learning.去重跳过', {
        existingId: duplicate.id,
        contentPreview: normalizedContent.substring(0, 60),
      }, LogCategory.SESSION);
      return { status: 'duplicate', record: duplicate };
    }

    const now = Date.now();
    const record: LearningRecord = {
      id: `learning_${now}_${Math.random().toString(36).substring(2, 8)}`,
      content: normalizedContent,
      context: normalizedContext,
      createdAt: now,
      tags: this.normalizeLearningTags(tags),
    };
    this.learnings.push(record);
    // 超出容量限制时淘汰最旧的记录
    if (this.learnings.length > ProjectKnowledgeBase.MAX_LEARNINGS) {
      const removed = this.learnings.shift();
      logger.info('项目知识库.Learning.容量淘汰', { removedId: removed?.id }, LogCategory.SESSION);
    }
    this.saveLearnings();
    logger.info('项目知识库.Learning.已添加', { id: record.id }, LogCategory.SESSION);
    return { status: 'inserted', record };
  }

  /**
   * 获取所有经验记录
   */
  getLearnings(): LearningRecord[] {
    const { records, changed } = this.normalizeLearningRecords(this.learnings);
    if (changed) {
      this.learnings = records;
      this.saveLearnings();
      logger.warn('项目知识库.Learning.已自动清理(访问时)', { count: this.learnings.length }, LogCategory.SESSION);
    }
    return this.learnings;
  }

  /**
   * 删除经验记录
   */
  deleteLearning(id: string): boolean {
    const index = this.learnings.findIndex(l => l.id === id);
    if (index === -1) {
      return false;
    }

    this.learnings.splice(index, 1);
    this.saveLearnings();
    logger.info('项目知识库.Learning.已删除', { id }, LogCategory.SESSION);
    return true;
  }

  // ============================================================================
  // 清空操作
  // ============================================================================

  /**
   * 清空所有知识（ADR + FAQ + Learning）
   */
  clearAll(): { adrs: number; faqs: number; learnings: number } {
    const counts = {
      adrs: this.adrs.length,
      faqs: this.faqs.length,
      learnings: this.learnings.length,
    };

    this.adrs = [];
    this.faqs = [];
    this.learnings = [];

    this.saveADRs();
    this.saveFAQs();
    this.saveLearnings();

    logger.info('项目知识库.已清空', counts, LogCategory.SESSION);
    return counts;
  }

  // ============================================================================
  // 本地搜索引擎
  // ============================================================================

  /**
   * 构建本地搜索引擎索引（从 codeIndex 的文件列表复用）
   * 首次调用创建实例，后续调用复用已有实例仅重建索引数据
   */
  private async buildSearchEngineIndex(): Promise<void> {
    if (!this.codeIndex || this.codeIndex.files.length === 0) return;

    try {
      // 复用已有实例，避免丢失增量更新和外部引用
      if (!this.localSearchEngine) {
        this.localSearchEngine = new LocalSearchEngine(this.projectRoot, this.searchEngineConfig);
      }

      // 传递 LLM 客户端用于查询扩展
      if (this.llmClient) {
        this.localSearchEngine.setLLMClient(this.llmClient);
      }

      await this.localSearchEngine.buildIndex(
        this.codeIndex.files.map(f => ({ path: f.path, type: f.type }))
      );
    } catch (error) {
      logger.warn('项目知识库.搜索引擎.构建失败', { error }, LogCategory.SESSION);
      this.localSearchEngine = null;
    }
  }

  // ============================================================================
  // 持久化
  // ============================================================================

  /**
   * 确保存储目录存在
   */
  private async ensureStorageDir(): Promise<void> {
    try {
      await fs.promises.access(this.storageDir);
    } catch {
      await fs.promises.mkdir(this.storageDir, { recursive: true });
      logger.info('项目知识库.存储目录.已创建', { dir: this.storageDir }, LogCategory.SESSION);
    }
  }

  /**
   * 保存代码索引（异步）
   */
  private async saveCodeIndex(): Promise<void> {
    if (!this.codeIndex) {
      return;
    }

    const filePath = path.join(this.storageDir, 'code-index.json');
    try {
      await this.ensureStorageDir();
      await fs.promises.writeFile(filePath, JSON.stringify(this.codeIndex, null, 2), 'utf-8');
      logger.info('项目知识库.代码索引.已保存', { path: filePath }, LogCategory.SESSION);
    } catch (error) {
      logger.error('项目知识库.代码索引.保存失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 加载代码索引（异步）
   */
  private async loadCodeIndex(): Promise<void> {
    const filePath = path.join(this.storageDir, 'code-index.json');
    try {
      const content = await fs.promises.readFile(filePath, 'utf-8');
      this.codeIndex = JSON.parse(content);
      logger.info('项目知识库.代码索引.已加载', {
        files: this.codeIndex?.files.length || 0
      }, LogCategory.SESSION);
    } catch {
      // 文件不存在或解析失败
    }
  }

  /**
   * 保存 ADRs（异步，fire-and-forget）
   */
  private saveADRs(): void {
    const filePath = path.join(this.storageDir, 'adrs.json');
    this.ensureStorageDir().then(() =>
      fs.promises.writeFile(filePath, JSON.stringify(this.adrs, null, 2), 'utf-8')
    ).then(() => {
      logger.info('项目知识库.ADR.已保存', { count: this.adrs.length }, LogCategory.SESSION);
    }).catch(error => {
      logger.error('项目知识库.ADR.保存失败', { error }, LogCategory.SESSION);
    });
  }

  /**
   * 加载 ADRs（异步）
   */
  private async loadADRs(): Promise<void> {
    const filePath = path.join(this.storageDir, 'adrs.json');
    try {
      const content = await fs.promises.readFile(filePath, 'utf-8');
      const raw = JSON.parse(content);
      const { records, changed } = this.normalizeADRRecords(raw);
      this.adrs = records;
      if (changed) {
        this.saveADRs();
        logger.warn('项目知识库.ADR.已自动清理', { count: this.adrs.length }, LogCategory.SESSION);
      } else {
        logger.info('项目知识库.ADR.已加载', { count: this.adrs.length }, LogCategory.SESSION);
      }
    } catch {
      // 文件不存在或解析失败
    }
  }

  /**
   * 保存 FAQs（异步，fire-and-forget）
   */
  private saveFAQs(): void {
    const filePath = path.join(this.storageDir, 'faqs.json');
    this.ensureStorageDir().then(() =>
      fs.promises.writeFile(filePath, JSON.stringify(this.faqs, null, 2), 'utf-8')
    ).then(() => {
      logger.info('项目知识库.FAQ.已保存', { count: this.faqs.length }, LogCategory.SESSION);
    }).catch(error => {
      logger.error('项目知识库.FAQ.保存失败', { error }, LogCategory.SESSION);
    });
  }

  /**
   * 保存经验记录（异步，fire-and-forget）
   */
  private saveLearnings(): void {
    const filePath = path.join(this.storageDir, 'learnings.json');
    this.ensureStorageDir().then(() =>
      fs.promises.writeFile(filePath, JSON.stringify(this.learnings, null, 2), 'utf-8')
    ).then(() => {
      logger.info('项目知识库.Learning.已保存', { count: this.learnings.length }, LogCategory.SESSION);
    }).catch(error => {
      logger.error('项目知识库.Learning.保存失败', { error }, LogCategory.SESSION);
    });
  }

  /**
   * 加载 FAQs（异步）
   */
  private async loadFAQs(): Promise<void> {
    const filePath = path.join(this.storageDir, 'faqs.json');
    try {
      const content = await fs.promises.readFile(filePath, 'utf-8');
      const raw = JSON.parse(content);
      const { records, changed } = this.normalizeFAQRecords(raw);
      this.faqs = records;
      if (changed) {
        this.saveFAQs();
        logger.warn('项目知识库.FAQ.已自动清理', { count: this.faqs.length }, LogCategory.SESSION);
      } else {
        logger.info('项目知识库.FAQ.已加载', { count: this.faqs.length }, LogCategory.SESSION);
      }
    } catch {
      // 文件不存在或解析失败
    }
  }

  /**
   * 加载经验记录（异步）
   */
  private async loadLearnings(): Promise<void> {
    const filePath = path.join(this.storageDir, 'learnings.json');
    try {
      const content = await fs.promises.readFile(filePath, 'utf-8');
      const raw = JSON.parse(content);
      const { records, changed } = this.normalizeLearningRecords(raw);
      this.learnings = records;
      if (changed) {
        this.saveLearnings();
        logger.warn('项目知识库.Learning.已自动清理', { count: this.learnings.length }, LogCategory.SESSION);
      } else {
        logger.info('项目知识库.Learning.已加载', { count: this.learnings.length }, LogCategory.SESSION);
      }
    } catch {
      // 文件不存在或解析失败
    }
  }

  private normalizeADRRecords(raw: unknown): { records: ADRRecord[]; changed: boolean } {
    const now = Date.now();
    if (!Array.isArray(raw)) {
      return { records: [], changed: true };
    }
    let changed = false;
    const records: ADRRecord[] = [];
    raw.forEach((item, index) => {
      if (!item || typeof item !== 'object') {
        changed = true;
        return;
      }
      const title = typeof (item as any).title === 'string' ? (item as any).title.trim() : '';
      if (!title) {
        changed = true;
        return;
      }
      const status = (item as any).status;
      const normalizedStatus: ADRStatus = status === 'accepted' || status === 'archived' || status === 'superseded'
        ? status
        : 'proposed';
      if (normalizedStatus !== status) changed = true;

      const dateValue = typeof (item as any).date === 'number' ? (item as any).date : now;
      if (dateValue !== (item as any).date) changed = true;

      const context = typeof (item as any).context === 'string' ? (item as any).context : '';
      const decision = typeof (item as any).decision === 'string' ? (item as any).decision : '';
      const consequences = typeof (item as any).consequences === 'string' ? (item as any).consequences : '';
      if (context !== (item as any).context || decision !== (item as any).decision || consequences !== (item as any).consequences) {
        changed = true;
      }

      const alternatives = Array.isArray((item as any).alternatives)
        ? (item as any).alternatives.filter((value: unknown) => typeof value === 'string')
        : [];
      const relatedFiles = Array.isArray((item as any).relatedFiles)
        ? (item as any).relatedFiles.filter((value: unknown) => typeof value === 'string')
        : [];
      if (alternatives.length !== ((item as any).alternatives || []).length || relatedFiles.length !== ((item as any).relatedFiles || []).length) {
        changed = true;
      }

      records.push({
        id: typeof (item as any).id === 'string' && (item as any).id ? (item as any).id : `adr-${now}-${index}`,
        title,
        date: dateValue,
        status: normalizedStatus,
        context,
        decision,
        consequences,
        alternatives,
        relatedFiles,
      });
      if (!((item as any).id)) changed = true;
    });
    return { records, changed };
  }

  private normalizeFAQRecords(raw: unknown): { records: FAQRecord[]; changed: boolean } {
    const now = Date.now();
    if (!Array.isArray(raw)) {
      return { records: [], changed: true };
    }
    let changed = false;
    const records: FAQRecord[] = [];
    raw.forEach((item, index) => {
      if (!item || typeof item !== 'object') {
        changed = true;
        return;
      }
      const question = typeof (item as any).question === 'string' ? (item as any).question.trim() : '';
      if (!question) {
        changed = true;
        return;
      }
      const answer = typeof (item as any).answer === 'string' ? (item as any).answer : '';
      const category = typeof (item as any).category === 'string' ? (item as any).category : 'general';
      const tags = Array.isArray((item as any).tags)
        ? (item as any).tags.filter((value: unknown) => typeof value === 'string')
        : [];
      const relatedFiles = Array.isArray((item as any).relatedFiles)
        ? (item as any).relatedFiles.filter((value: unknown) => typeof value === 'string')
        : [];
      const createdAt = typeof (item as any).createdAt === 'number' ? (item as any).createdAt : now;
      const updatedAt = typeof (item as any).updatedAt === 'number' ? (item as any).updatedAt : now;
      const useCount = typeof (item as any).useCount === 'number' ? (item as any).useCount : 0;

      if (
        answer !== (item as any).answer ||
        category !== (item as any).category ||
        tags.length !== ((item as any).tags || []).length ||
        relatedFiles.length !== ((item as any).relatedFiles || []).length ||
        createdAt !== (item as any).createdAt ||
        updatedAt !== (item as any).updatedAt ||
        useCount !== (item as any).useCount
      ) {
        changed = true;
      }

      records.push({
        id: typeof (item as any).id === 'string' && (item as any).id ? (item as any).id : `faq-${now}-${index}`,
        question,
        answer,
        category,
        tags,
        relatedFiles,
        createdAt,
        updatedAt,
        useCount,
      });
      if (!((item as any).id)) changed = true;
    });
    return { records, changed };
  }

  private normalizeLearningRecords(raw: unknown): { records: LearningRecord[]; changed: boolean } {
    const now = Date.now();
    if (!Array.isArray(raw)) {
      return { records: [], changed: true };
    }

    let changed = false;
    const records: LearningRecord[] = [];
    raw.forEach((item, index) => {
      if (!item || typeof item !== 'object') {
        changed = true;
        return;
      }
      const content = this.sanitizeLearningContent(typeof (item as any).content === 'string' ? (item as any).content : '');
      if (!this.isLearningContentQualified(content)) {
        changed = true;
        return;
      }
      const context = this.sanitizeLearningContext(typeof (item as any).context === 'string' ? (item as any).context : '');
      const createdAt = typeof (item as any).createdAt === 'number' ? (item as any).createdAt : now;
      const tags = this.normalizeLearningTags((item as any).tags);

      const duplicate = records.find((record) => this.isLearningDuplicate(content, context, record.content, record.context));
      if (duplicate) {
        changed = true;
        return;
      }

      if (
        context !== (item as any).context ||
        createdAt !== (item as any).createdAt ||
        (Array.isArray(tags) && tags.length !== ((item as any).tags || []).length)
      ) {
        changed = true;
      }

      records.push({
        id: typeof (item as any).id === 'string' && (item as any).id ? (item as any).id : `learning-${now}-${index}`,
        content,
        context,
        createdAt,
        tags,
      });
      if (!((item as any).id)) changed = true;
    });
    return { records, changed };
  }

  private sanitizeLearningContent(content: string): string {
    if (typeof content !== 'string') return '';
    return content
      .replace(/^[\s\-*•\d.)]+/, '')
      .replace(/\s+/g, ' ')
      .trim()
      .slice(0, ProjectKnowledgeBase.MAX_LEARNING_CONTENT_LENGTH);
  }

  private sanitizeLearningContext(context: string): string {
    if (typeof context !== 'string') return '';
    return context.replace(/\s+/g, ' ').trim().slice(0, 300);
  }

  private normalizeLearningTags(tags: unknown): string[] | undefined {
    if (!Array.isArray(tags)) return undefined;
    const normalized = Array.from(new Set(
      tags
        .filter((value): value is string => typeof value === 'string')
        .map((tag) => tag.trim())
        .filter((tag) => tag.length > 0)
        .slice(0, 8)
    ));
    return normalized.length > 0 ? normalized : undefined;
  }

  private isLearningContentQualified(content: string): boolean {
    if (!content) return false;
    if (content.length < ProjectKnowledgeBase.MIN_LEARNING_CONTENT_LENGTH) return false;
    if (ProjectKnowledgeBase.LOW_VALUE_LEARNING_PATTERNS.some((pattern) => pattern.test(content.toLowerCase()))) {
      return false;
    }
    // 至少包含一个字母/数字/汉字，避免纯符号文本
    if (!/[a-zA-Z0-9\u4e00-\u9fa5]/.test(content)) return false;
    return true;
  }

  private normalizeLearningTextForDedup(text: string): string {
    if (typeof text !== 'string') return '';
    return text
      .toLowerCase()
      .replace(/[，。！？、；：,.!?;:()[\]{}"'`~@#$%^&*_+=<>|\\/]/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
  }

  private isLearningDuplicate(
    contentA: string,
    contextA: string,
    contentB: string,
    contextB: string
  ): boolean {
    const normalizedA = this.normalizeLearningTextForDedup(contentA);
    const normalizedB = this.normalizeLearningTextForDedup(contentB);
    if (!normalizedA || !normalizedB) return false;

    if (normalizedA === normalizedB) return true;
    if (
      normalizedA.length >= 18
      && normalizedB.length >= 18
      && (normalizedA.includes(normalizedB) || normalizedB.includes(normalizedA))
    ) {
      return true;
    }
    if (this.textSimilarity(normalizedA, normalizedB) >= 0.85) return true;

    // 同上下文下的高相似内容视为重复
    const normalizedCtxA = this.normalizeLearningTextForDedup(contextA);
    const normalizedCtxB = this.normalizeLearningTextForDedup(contextB);
    if (normalizedCtxA && normalizedCtxA === normalizedCtxB && this.textSimilarity(normalizedA, normalizedB) >= 0.75) {
      return true;
    }
    return false;
  }

  private findDuplicateLearning(content: string, context: string): LearningRecord | undefined {
    return this.learnings.find((record) =>
      this.isLearningDuplicate(content, context, record.content, record.context)
    );
  }

  /**
   * 文本相似度计算（基于 bigram 重叠率）
   * 返回 0~1 之间的相似度值
   */
  private textSimilarity(a: string, b: string): number {
    const normalize = (s: string) => s.toLowerCase().replace(/\s+/g, ' ').trim();
    const na = normalize(a);
    const nb = normalize(b);
    if (na === nb) return 1;
    if (na.length < 2 || nb.length < 2) return 0;

    const bigrams = (s: string): Set<string> => {
      const set = new Set<string>();
      for (let i = 0; i < s.length - 1; i++) {
        set.add(s.substring(i, i + 2));
      }
      return set;
    };

    const setA = bigrams(na);
    const setB = bigrams(nb);
    let intersection = 0;
    for (const bg of setA) {
      if (setB.has(bg)) intersection++;
    }
    return (2 * intersection) / (setA.size + setB.size);
  }
}
