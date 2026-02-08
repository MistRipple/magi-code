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
import { logger, LogCategory } from '../logging';
import { LLMConfigLoader } from '../llm/config';
import { LLMClient, LLMMessageParams } from '../llm/types';

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

/**
 * 项目知识库配置
 */
export interface ProjectKnowledgeConfig {
  projectRoot: string;
  storageDir?: string;  // 默认 .multicli/knowledge
  indexPatterns?: string[];  // 要索引的文件模式
  ignorePatterns?: string[]; // 要忽略的文件模式
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

  private llmClient: LLMClient | null = null;

  constructor(config: ProjectKnowledgeConfig) {
    this.projectRoot = config.projectRoot;
    this.projectName = path.basename(this.projectRoot);
    this.storageDir = config.storageDir || path.join(this.projectRoot, '.multicli', 'knowledge');

    // 默认索引模式
    this.indexPatterns = config.indexPatterns || [
      '**/*.ts',
      '**/*.js',
      '**/*.tsx',
      '**/*.jsx',
      '**/*.json',
      '**/*.md',
      '**/*.yml',
      '**/*.yaml'
    ];

    // 默认忽略模式
    this.ignorePatterns = config.ignorePatterns || [
      '**/node_modules/**',
      '**/dist/**',
      '**/out/**',
      '**/build/**',
      '**/.git/**',
      '**/.vscode/**',
      '**/coverage/**',
      '**/*.min.js',
      '**/*.map'
    ];
  }

  /**
   * 初始化知识库
   * 加载已有的索引、ADR、FAQ
   */
  async initialize(): Promise<void> {
    logger.info('项目知识库.初始化.开始', { projectRoot: this.projectRoot }, LogCategory.SESSION);

    // 确保存储目录存在
    this.ensureStorageDir();

    // 加载已有数据
    await this.loadCodeIndex();
    await this.loadADRs();
    await this.loadFAQs();
    await this.loadLearnings();

    // 如果没有索引，执行首次索引
    if (!this.codeIndex) {
      await this.indexProject();
    }

    logger.info('项目知识库.初始化.完成', {
      files: this.codeIndex?.files.length || 0,
      adrs: this.adrs.length,
      faqs: this.faqs.length,
      learnings: this.learnings.length
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
   * 扫描文件
   */
  private async scanFiles(): Promise<FileEntry[]> {
    const files: FileEntry[] = [];

    // 递归扫描目录
    const scanDirectory = (dir: string) => {
      try {
        const entries = fs.readdirSync(dir, { withFileTypes: true });

        for (const entry of entries) {
          const fullPath = path.join(dir, entry.name);
          const relativePath = path.relative(this.projectRoot, fullPath);

          // 检查是否应该忽略
          if (this.shouldIgnore(relativePath)) {
            continue;
          }

          if (entry.isDirectory()) {
            scanDirectory(fullPath);
          } else if (entry.isFile()) {
            // 检查文件扩展名是否匹配
            if (this.shouldIndex(relativePath)) {
              const stats = fs.statSync(fullPath);
              files.push({
                path: relativePath,
                type: this.classifyFileType(relativePath),
                language: this.detectLanguage(relativePath),
                size: stats.size
              });
            }
          }
        }
      } catch (error) {
        logger.warn('项目知识库.扫描目录.失败', { dir, error }, LogCategory.SESSION);
      }
    };

    scanDirectory(this.projectRoot);
    return files;
  }

  /**
   * 检查文件是否应该被索引
   */
  private shouldIndex(filePath: string): boolean {
    const ext = path.extname(filePath);
    const validExts = ['.ts', '.js', '.tsx', '.jsx', '.json', '.md', '.yml', '.yaml'];
    return validExts.includes(ext);
  }

  /**
   * 检查路径是否应该被忽略
   */
  private shouldIgnore(filePath: string): boolean {
    const ignorePatterns = [
      'node_modules',
      'dist',
      'out',
      'build',
      '.git',
      '.vscode',
      'coverage',
      '.min.js',
      '.map'
    ];

    return ignorePatterns.some(pattern => filePath.includes(pattern));
  }

  /**
   * 扫描目录
   */
  private async scanDirectories(): Promise<DirectoryEntry[]> {
    const directories: DirectoryEntry[] = [];

    const scanDirectory = (dir: string) => {
      try {
        const relativePath = path.relative(this.projectRoot, dir);

        // 跳过根目录和忽略的目录
        if (relativePath && !this.shouldIgnore(relativePath)) {
          const entries = fs.readdirSync(dir, { withFileTypes: true });

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
        const entries = fs.readdirSync(dir, { withFileTypes: true });
        for (const entry of entries) {
          if (entry.isDirectory()) {
            const fullPath = path.join(dir, entry.name);
            const relativePath = path.relative(this.projectRoot, fullPath);
            if (!this.shouldIgnore(relativePath)) {
              scanDirectory(fullPath);
            }
          }
        }
      } catch (error) {
        logger.warn('项目知识库.扫描目录.失败', { dir, error }, LogCategory.SESSION);
      }
    };

    scanDirectory(this.projectRoot);
    return directories;
  }

  /**
   * 检测技术栈
   */
  private async detectTechStack(): Promise<TechStack> {
    const techStack: TechStack = {
      languages: [],
      frameworks: [],
      buildTools: [],
      testFrameworks: []
    };

    // 检测语言
    const packageJsonPath = path.join(this.projectRoot, 'package.json');
    const tsconfigPath = path.join(this.projectRoot, 'tsconfig.json');

    if (fs.existsSync(tsconfigPath)) {
      techStack.languages.push('TypeScript');
    }
    if (fs.existsSync(packageJsonPath)) {
      techStack.languages.push('JavaScript');
    }

    // 读取 package.json 检测框架和工具
    if (fs.existsSync(packageJsonPath)) {
      try {
        const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, 'utf-8'));
        const allDeps = {
          ...packageJson.dependencies,
          ...packageJson.devDependencies
        };

        // 检测框架
        if (allDeps['react']) techStack.frameworks.push('React');
        if (allDeps['vue']) techStack.frameworks.push('Vue');
        if (allDeps['@angular/core']) techStack.frameworks.push('Angular');
        if (allDeps['express']) techStack.frameworks.push('Express');
        if (allDeps['vscode']) techStack.frameworks.push('VSCode Extension');

        // 检测构建工具
        if (allDeps['webpack']) techStack.buildTools.push('Webpack');
        if (allDeps['vite']) techStack.buildTools.push('Vite');
        if (allDeps['rollup']) techStack.buildTools.push('Rollup');
        if (allDeps['esbuild']) techStack.buildTools.push('esbuild');
        if (packageJson.scripts?.build) techStack.buildTools.push('npm scripts');

        // 检测测试框架
        if (allDeps['jest']) techStack.testFrameworks.push('Jest');
        if (allDeps['mocha']) techStack.testFrameworks.push('Mocha');
        if (allDeps['vitest']) techStack.testFrameworks.push('Vitest');
        if (allDeps['@playwright/test']) techStack.testFrameworks.push('Playwright');
      } catch (error) {
        logger.warn('项目知识库.技术栈检测.失败', { error }, LogCategory.SESSION);
      }
    }

    return techStack;
  }

  /**
   * 读取依赖信息
   */
  private async readDependencies(): Promise<DependencyInfo> {
    const packageJsonPath = path.join(this.projectRoot, 'package.json');

    if (!fs.existsSync(packageJsonPath)) {
      return {
        dependencies: {},
        devDependencies: {}
      };
    }

    try {
      const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, 'utf-8'));
      return {
        dependencies: packageJson.dependencies || {},
        devDependencies: packageJson.devDependencies || {}
      };
    } catch (error) {
      logger.warn('项目知识库.依赖读取.失败', { error }, LogCategory.SESSION);
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
        logger.info('项目知识库.索引.刷新完成', {
          files: this.codeIndex?.files.length || 0,
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
    const estimatedTokens = Math.ceil(context.length / 4);

    if (estimatedTokens > maxTokens) {
      // 截断到最大 tokens
      const maxChars = maxTokens * 4;
      return context.substring(0, maxChars) + '...';
    }

    return context;
  }

  /**
   * 设置 LLM 客户端（用于自动知识提取）
   */
  setLLMClient(client: LLMClient): void {
    this.llmClient = client;
    logger.info('项目知识库.LLM客户端.已设置', undefined, LogCategory.SESSION);
  }

  /**
   * 从会话消息中提取 ADR
   * 使用压缩模型进行智能提取
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
   * 使用压缩模型进行智能提取
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
   * 从 LLM 响应中解析 ADR
   */
  private parseADRsFromResponse(response: string): ADRRecord[] {
    try {
      // 提取 JSON 内容
      const jsonMatch = response.match(/```json\s*([\s\S]*?)\s*```/) || response.match(/\[([\s\S]*?)\]/);
      if (!jsonMatch) {
        logger.warn('项目知识库.ADR解析.未找到JSON', undefined, LogCategory.SESSION);
        return [];
      }

      const jsonText = jsonMatch[1] || jsonMatch[0];
      const extractedADRs = JSON.parse(jsonText);

      if (!Array.isArray(extractedADRs)) {
        return [];
      }

      // 转换为 ADRRecord 格式
      return extractedADRs.map((adr, index) => ({
        id: `adr-${Date.now()}-${index}`,
        title: adr.title || '未命名决策',
        date: Date.now(),
        status: 'proposed' as ADRStatus,
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
      // 提取 JSON 内容
      const jsonMatch = response.match(/```json\s*([\s\S]*?)\s*```/) || response.match(/\[([\s\S]*?)\]/);
      if (!jsonMatch) {
        logger.warn('项目知识库.FAQ解析.未找到JSON', undefined, LogCategory.SESSION);
        return [];
      }

      const jsonText = jsonMatch[1] || jsonMatch[0];
      const extractedFAQs = JSON.parse(jsonText);

      if (!Array.isArray(extractedFAQs)) {
        return [];
      }

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

  // ============================================================================
  // ADR 管理
  // ============================================================================

  /**
   * 添加 ADR
   */
  addADR(adr: ADRRecord): void {
    this.adrs.push(adr);
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
   * 添加 FAQ
   */
  addFAQ(faq: FAQRecord): void {
    this.faqs.push(faq);
    this.saveFAQs();
    logger.info('项目知识库.FAQ.已添加', { id: faq.id, question: faq.question }, LogCategory.SESSION);
  }

  /**
   * 搜索 FAQs
   */
  searchFAQs(keyword: string): FAQRecord[] {
    const lowerKeyword = keyword.toLowerCase();
    return this.faqs.filter(faq => {
      return (
        faq.question.toLowerCase().includes(lowerKeyword) ||
        faq.answer.toLowerCase().includes(lowerKeyword) ||
        faq.tags.some(tag => tag.toLowerCase().includes(lowerKeyword))
      );
    });
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
  addLearning(content: string, context: string, tags?: string[]): LearningRecord {
    const now = Date.now();
    const record: LearningRecord = {
      id: `learning_${now}_${Math.random().toString(36).substring(2, 8)}`,
      content,
      context,
      createdAt: now,
      tags,
    };
    this.learnings.push(record);
    this.saveLearnings();
    logger.info('项目知识库.Learning.已添加', { id: record.id }, LogCategory.SESSION);
    return record;
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
  // 持久化
  // ============================================================================

  /**
   * 确保存储目录存在
   */
  private ensureStorageDir(): void {
    if (!fs.existsSync(this.storageDir)) {
      fs.mkdirSync(this.storageDir, { recursive: true });
      logger.info('项目知识库.存储目录.已创建', { dir: this.storageDir }, LogCategory.SESSION);
    }
  }

  /**
   * 保存代码索引
   */
  private async saveCodeIndex(): Promise<void> {
    if (!this.codeIndex) {
      return;
    }

    const filePath = path.join(this.storageDir, 'code-index.json');
    try {
      fs.writeFileSync(filePath, JSON.stringify(this.codeIndex, null, 2), 'utf-8');
      logger.info('项目知识库.代码索引.已保存', { path: filePath }, LogCategory.SESSION);
    } catch (error) {
      logger.error('项目知识库.代码索引.保存失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 加载代码索引
   */
  private async loadCodeIndex(): Promise<void> {
    const filePath = path.join(this.storageDir, 'code-index.json');
    if (!fs.existsSync(filePath)) {
      return;
    }

    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      this.codeIndex = JSON.parse(content);
      logger.info('项目知识库.代码索引.已加载', {
        files: this.codeIndex?.files.length || 0
      }, LogCategory.SESSION);
    } catch (error) {
      logger.error('项目知识库.代码索引.加载失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 保存 ADRs
   */
  private saveADRs(): void {
    const filePath = path.join(this.storageDir, 'adrs.json');
    try {
      fs.writeFileSync(filePath, JSON.stringify(this.adrs, null, 2), 'utf-8');
      logger.info('项目知识库.ADR.已保存', { count: this.adrs.length }, LogCategory.SESSION);
    } catch (error) {
      logger.error('项目知识库.ADR.保存失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 加载 ADRs
   */
  private async loadADRs(): Promise<void> {
    const filePath = path.join(this.storageDir, 'adrs.json');
    if (!fs.existsSync(filePath)) {
      return;
    }

    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      const raw = JSON.parse(content);
      const { records, changed } = this.normalizeADRRecords(raw);
      this.adrs = records;
      if (changed) {
        this.saveADRs();
        logger.warn('项目知识库.ADR.已自动清理', { count: this.adrs.length }, LogCategory.SESSION);
      } else {
        logger.info('项目知识库.ADR.已加载', { count: this.adrs.length }, LogCategory.SESSION);
      }
    } catch (error) {
      logger.error('项目知识库.ADR.加载失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 保存 FAQs
   */
  private saveFAQs(): void {
    const filePath = path.join(this.storageDir, 'faqs.json');
    try {
      fs.writeFileSync(filePath, JSON.stringify(this.faqs, null, 2), 'utf-8');
      logger.info('项目知识库.FAQ.已保存', { count: this.faqs.length }, LogCategory.SESSION);
    } catch (error) {
      logger.error('项目知识库.FAQ.保存失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 保存经验记录
   */
  private saveLearnings(): void {
    const filePath = path.join(this.storageDir, 'learnings.json');
    try {
      fs.writeFileSync(filePath, JSON.stringify(this.learnings, null, 2), 'utf-8');
      logger.info('项目知识库.Learning.已保存', { count: this.learnings.length }, LogCategory.SESSION);
    } catch (error) {
      logger.error('项目知识库.Learning.保存失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 加载 FAQs
   */
  private async loadFAQs(): Promise<void> {
    const filePath = path.join(this.storageDir, 'faqs.json');
    if (!fs.existsSync(filePath)) {
      return;
    }

    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      const raw = JSON.parse(content);
      const { records, changed } = this.normalizeFAQRecords(raw);
      this.faqs = records;
      if (changed) {
        this.saveFAQs();
        logger.warn('项目知识库.FAQ.已自动清理', { count: this.faqs.length }, LogCategory.SESSION);
      } else {
        logger.info('项目知识库.FAQ.已加载', { count: this.faqs.length }, LogCategory.SESSION);
      }
    } catch (error) {
      logger.error('项目知识库.FAQ.加载失败', { error }, LogCategory.SESSION);
    }
  }

  /**
   * 加载经验记录
   */
  private async loadLearnings(): Promise<void> {
    const filePath = path.join(this.storageDir, 'learnings.json');
    if (!fs.existsSync(filePath)) {
      return;
    }

    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      const raw = JSON.parse(content);
      const { records, changed } = this.normalizeLearningRecords(raw);
      this.learnings = records;
      if (changed) {
        this.saveLearnings();
        logger.warn('项目知识库.Learning.已自动清理', { count: this.learnings.length }, LogCategory.SESSION);
      } else {
        logger.info('项目知识库.Learning.已加载', { count: this.learnings.length }, LogCategory.SESSION);
      }
    } catch (error) {
      logger.error('项目知识库.Learning.加载失败', { error }, LogCategory.SESSION);
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
      const content = typeof (item as any).content === 'string' ? (item as any).content.trim() : '';
      if (!content) {
        changed = true;
        return;
      }
      const context = typeof (item as any).context === 'string' ? (item as any).context : '';
      const createdAt = typeof (item as any).createdAt === 'number' ? (item as any).createdAt : now;
      const tags = Array.isArray((item as any).tags)
        ? (item as any).tags.filter((value: unknown) => typeof value === 'string')
        : undefined;

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
}
