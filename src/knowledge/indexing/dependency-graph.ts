/**
 * DependencyGraph — 依赖图谱
 *
 * 通过解析 import/require 语句构建文件间依赖关系图，
 * 支持：
 * - 正向依赖查询：文件 A 依赖了哪些文件
 * - 反向依赖查询：哪些文件依赖了文件 A
 * - 依赖深度遍历：从文件 A 出发，沿依赖关系展开 N 层
 * - 中心度计算：哪些文件是项目的核心枢纽
 */

import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../../logging';

// ============================================================================
// 类型定义
// ============================================================================

/** 依赖边 */
export interface DependencyEdge {
  /** 来源文件 */
  from: string;
  /** 目标文件 */
  to: string;
  /** 导入类型 */
  importType: 'static' | 'dynamic' | 'require';
  /** 导入的具体符号 */
  importedNames?: string[];
}

/** 文件中心度信息 */
export interface FileCentrality {
  filePath: string;
  /** 入度：被多少文件依赖 */
  inDegree: number;
  /** 出度：依赖了多少文件 */
  outDegree: number;
  /** 综合中心度得分（0-1） */
  centrality: number;
}

/** 依赖图序列化快照 */
export interface DependencyGraphSnapshot {
  forwardDeps: Array<[string, string[]]>;
  reverseDeps: Array<[string, string[]]>;
  edges: DependencyEdge[];
  centralityCache: Array<[string, number]>;
}

// ============================================================================
// import/require 解析正则
// ============================================================================

/** ES import 模式 */
const IMPORT_PATTERNS = [
  // import { xxx } from './path'
  /^import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]/gm,
  // import xxx from './path'
  /^import\s+([a-zA-Z_$][a-zA-Z0-9_$]*)\s+from\s+['"]([^'"]+)['"]/gm,
  // import * as xxx from './path'
  /^import\s+\*\s+as\s+([a-zA-Z_$][a-zA-Z0-9_$]*)\s+from\s+['"]([^'"]+)['"]/gm,
  // import './path' (side effect)
  /^import\s+['"]([^'"]+)['"]/gm,
];

/** re-export 模式（export * from / export { x } from） */
const REEXPORT_PATTERNS = [
  // export * from './path'
  /^export\s+\*\s+from\s+['"]([^'"]+)['"]/gm,
  // export { x, y } from './path'
  /^export\s+\{[^}]*\}\s+from\s+['"]([^'"]+)['"]/gm,
];

/** require 模式 */
const REQUIRE_PATTERN = /require\s*\(\s*['"]([^'"]+)['"]\s*\)/gm;

/** dynamic import 模式 */
const DYNAMIC_IMPORT_PATTERN = /import\s*\(\s*['"]([^'"]+)['"]\s*\)/gm;

/** Python import 模式 */
const PY_IMPORT_PATTERNS = [
  // from .module import x / from . import module
  /^from\s+(\.+\w*)\s+import\s+/gm,
  // from ..module import x
  /^from\s+(\.{2,}\w*)\s+import\s+/gm,
];

// ============================================================================
// DependencyGraph 类
// ============================================================================

export class DependencyGraph {
  /** 正向依赖：文件 → 它依赖的文件列表 */
  private forwardDeps = new Map<string, Set<string>>();
  /** 反向依赖：文件 → 依赖它的文件列表 */
  private reverseDeps = new Map<string, Set<string>>();
  /** 所有依赖边 */
  private edges: DependencyEdge[] = [];
  /** 中心度缓存 */
  private centralityCache = new Map<string, number>();
  /** 已知文件集合（用于增量更新时解析路径） */
  private fileSet = new Set<string>();
  /** 项目根目录 */
  private _projectRoot = '';
  /** 优化 #11: tsconfig paths 别名映射 */
  private pathAliases = new Map<string, string>();
  /** 优化 #21: 中心度防抖重算定时器 */
  private centralityDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  private static readonly CENTRALITY_DEBOUNCE_MS = 2000;
  private _isReady = false;

  get isReady(): boolean {
    return this._isReady;
  }

  /**
   * 从文件列表构建依赖图
   */
  async buildFromFiles(
    projectRoot: string,
    files: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>
  ): Promise<void> {
    this.clear();
    this._projectRoot = projectRoot;
    const startTime = Date.now();

    // 优化 #11: 解析 tsconfig paths 别名
    this.loadTsconfigPaths(projectRoot);

    // 建立文件路径集合（用于解析相对路径 + 增量更新复用）
    this.fileSet = new Set(files.map(f => f.path));
    const sourceFiles = files.filter(f => f.type === 'source' || f.type === 'test');

    for (const file of sourceFiles) {
      try {
        const fullPath = path.join(projectRoot, file.path);
        if (!fs.existsSync(fullPath)) continue;
        const stat = fs.statSync(fullPath);
        if (stat.size > 500 * 1024) continue;

        const content = fs.readFileSync(fullPath, 'utf-8');
        this.parseImports(file.path, content, projectRoot, this.fileSet);
      } catch {
        // 跳过
      }
    }

    // 计算中心度
    this.computeCentrality();

    this._isReady = true;
    const elapsed = Date.now() - startTime;

    logger.info('依赖图谱.构建完成', {
      files: sourceFiles.length,
      edges: this.edges.length,
      elapsed: `${elapsed}ms`,
    }, LogCategory.SESSION);
  }

  /**
   * 获取文件的正向依赖（它依赖了哪些文件）
   */
  getDependencies(filePath: string): string[] {
    return Array.from(this.forwardDeps.get(filePath) || []);
  }

  /**
   * 获取文件的反向依赖（哪些文件依赖了它）
   */
  getDependents(filePath: string): string[] {
    return Array.from(this.reverseDeps.get(filePath) || []);
  }

  /**
   * 获取文件的中心度得分（0-1）
   */
  getCentrality(filePath: string): number {
    return this.centralityCache.get(filePath) || 0;
  }

  /**
   * 从文件出发，沿依赖关系展开 N 层，收集相关文件
   * @param filePath 起点文件
   * @param depth 展开深度（默认 2）
   * @param direction 展开方向
   */
  expand(
    filePath: string,
    depth = 2,
    direction: 'forward' | 'reverse' | 'both' = 'both'
  ): string[] {
    const visited = new Set<string>();
    const queue: Array<{ file: string; level: number }> = [{ file: filePath, level: 0 }];

    while (queue.length > 0) {
      const { file, level } = queue.shift()!;
      if (visited.has(file) || level > depth) continue;
      visited.add(file);

      if (direction === 'forward' || direction === 'both') {
        for (const dep of this.getDependencies(file)) {
          if (!visited.has(dep)) queue.push({ file: dep, level: level + 1 });
        }
      }
      if (direction === 'reverse' || direction === 'both') {
        for (const dep of this.getDependents(file)) {
          if (!visited.has(dep)) queue.push({ file: dep, level: level + 1 });
        }
      }
    }

    // 移除起点自身
    visited.delete(filePath);
    return Array.from(visited);
  }

  /**
   * 获取中心度排名前 N 的文件
   */
  getTopCentralFiles(n = 10): FileCentrality[] {
    const results: FileCentrality[] = [];
    const allFiles = new Set([...this.forwardDeps.keys(), ...this.reverseDeps.keys()]);

    for (const filePath of allFiles) {
      results.push({
        filePath,
        inDegree: this.reverseDeps.get(filePath)?.size || 0,
        outDegree: this.forwardDeps.get(filePath)?.size || 0,
        centrality: this.centralityCache.get(filePath) || 0,
      });
    }

    return results
      .sort((a, b) => b.centrality - a.centrality)
      .slice(0, n);
  }

  /**
   * 获取统计信息
   */
  getStats(): { totalFiles: number; totalEdges: number; isReady: boolean } {
    const allFiles = new Set([...this.forwardDeps.keys(), ...this.reverseDeps.keys()]);
    return {
      totalFiles: allFiles.size,
      totalEdges: this.edges.length,
      isReady: this._isReady,
    };
  }

  /**
   * 清空图谱
   */
  clear(): void {
    if (this.centralityDebounceTimer) {
      clearTimeout(this.centralityDebounceTimer);
      this.centralityDebounceTimer = null;
    }
    this.forwardDeps.clear();
    this.reverseDeps.clear();
    this.edges = [];
    this.centralityCache.clear();
    this.fileSet.clear();
    this.pathAliases.clear();
    this._projectRoot = '';
    this._isReady = false;
  }

  // ==========================================================================
  // 序列化 / 反序列化
  // ==========================================================================

  /**
   * 序列化为 JSON 可存储对象
   */
  toJSON(): DependencyGraphSnapshot {
    return {
      forwardDeps: Array.from(this.forwardDeps.entries()).map(
        ([k, v]) => [k, Array.from(v)]
      ),
      reverseDeps: Array.from(this.reverseDeps.entries()).map(
        ([k, v]) => [k, Array.from(v)]
      ),
      edges: this.edges,
      centralityCache: Array.from(this.centralityCache.entries()),
    };
  }

  /**
   * 从序列化数据恢复图谱
   */
  fromJSON(snapshot: DependencyGraphSnapshot, projectRoot: string, fileSet: Set<string>): void {
    this.clear();
    this._projectRoot = projectRoot;
    this.fileSet = fileSet;

    for (const [k, v] of snapshot.forwardDeps) {
      this.forwardDeps.set(k, new Set(v));
    }
    for (const [k, v] of snapshot.reverseDeps) {
      this.reverseDeps.set(k, new Set(v));
    }
    this.edges = snapshot.edges;
    this.centralityCache = new Map(snapshot.centralityCache);
    this._isReady = true;
  }

  // ==========================================================================
  // 增量更新
  // ==========================================================================

  /**
   * 从图谱中移除一个文件及其所有关联边
   */
  removeFile(filePath: string): void {
    // 1. 移除该文件的正向边（它 → 别人）
    const forwardTargets = this.forwardDeps.get(filePath);
    if (forwardTargets) {
      for (const target of forwardTargets) {
        const reverseSet = this.reverseDeps.get(target);
        if (reverseSet) {
          reverseSet.delete(filePath);
          if (reverseSet.size === 0) this.reverseDeps.delete(target);
        }
      }
      this.forwardDeps.delete(filePath);
    }

    // 2. 移除该文件的反向边（别人 → 它）
    const reverseSources = this.reverseDeps.get(filePath);
    if (reverseSources) {
      for (const source of reverseSources) {
        const forwardSet = this.forwardDeps.get(source);
        if (forwardSet) {
          forwardSet.delete(filePath);
          if (forwardSet.size === 0) this.forwardDeps.delete(source);
        }
      }
      this.reverseDeps.delete(filePath);
    }

    // 3. 从边列表中过滤
    this.edges = this.edges.filter(e => e.from !== filePath && e.to !== filePath);

    // 4. 移除中心度缓存
    this.centralityCache.delete(filePath);

    // 5. 从文件集合中移除
    this.fileSet.delete(filePath);

    // 6. 防抖重算中心度（避免高频编辑时反复计算）
    this.debouncedComputeCentrality();
  }

  /**
   * 增量更新单个文件的依赖关系
   * 先清除旧边，再重新解析 imports 建立新边
   */
  updateFile(projectRoot: string, filePath: string): void {
    // 移除该文件的旧边（只移除正向边，保留其他文件指向它的边）
    const oldForwardTargets = this.forwardDeps.get(filePath);
    if (oldForwardTargets) {
      for (const target of oldForwardTargets) {
        const reverseSet = this.reverseDeps.get(target);
        if (reverseSet) {
          reverseSet.delete(filePath);
          if (reverseSet.size === 0) this.reverseDeps.delete(target);
        }
      }
      this.forwardDeps.delete(filePath);
    }
    // 移除旧的 from 边
    this.edges = this.edges.filter(e => e.from !== filePath);

    // 重新解析文件
    this.fileSet.add(filePath);
    this._projectRoot = projectRoot;

    try {
      const fullPath = path.join(projectRoot, filePath);
      if (!fs.existsSync(fullPath)) return;
      const stat = fs.statSync(fullPath);
      if (stat.size > 500 * 1024) return;

      const content = fs.readFileSync(fullPath, 'utf-8');
      this.parseImports(filePath, content, projectRoot, this.fileSet);
    } catch {
      // 文件读取失败，跳过
    }

    // 防抖重算中心度
    this.debouncedComputeCentrality();
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  /**
   * 优化 #21: 防抖中心度重算
   * 合并高频文件变更事件，避免每次文件保存都触发完整 PageRank 迭代
   */
  private debouncedComputeCentrality(): void {
    if (this.centralityDebounceTimer) {
      clearTimeout(this.centralityDebounceTimer);
    }
    this.centralityDebounceTimer = setTimeout(() => {
      this.centralityDebounceTimer = null;
      this.computeCentrality();
    }, DependencyGraph.CENTRALITY_DEBOUNCE_MS);
  }

  /**
   * 解析文件的 import/require 语句
   */
  private parseImports(
    filePath: string,
    content: string,
    projectRoot: string,
    fileSet: Set<string>
  ): void {
    const lines = content.split('\n');
    const ext = path.extname(filePath);
    const isPython = ext === '.py';

    for (const line of lines) {
      const trimmed = line.trimStart();
      // 跳过注释
      if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')) continue;
      if (isPython && trimmed.startsWith('#')) continue;

      // ES static imports
      for (const pattern of IMPORT_PATTERNS) {
        pattern.lastIndex = 0;
        const match = pattern.exec(line);
        if (match) {
          // 根据模式不同，模块路径在不同位置
          const modulePath = match[2] || match[1];
          if (modulePath && this.isInternalPath(modulePath)) {
            const resolved = this.resolveModulePath(filePath, modulePath, projectRoot, fileSet);
            if (resolved) {
              this.addEdge(filePath, resolved, 'static');
            }
          }
        }
      }

      // re-export（export * from / export { x } from）
      for (const pattern of REEXPORT_PATTERNS) {
        pattern.lastIndex = 0;
        const match = pattern.exec(line);
        if (match) {
          const modulePath = match[1];
          if (modulePath && this.isInternalPath(modulePath)) {
            const resolved = this.resolveModulePath(filePath, modulePath, projectRoot, fileSet);
            if (resolved) {
              this.addEdge(filePath, resolved, 'static');
            }
          }
        }
      }

      // require()
      REQUIRE_PATTERN.lastIndex = 0;
      let requireMatch;
      while ((requireMatch = REQUIRE_PATTERN.exec(line)) !== null) {
        const modulePath = requireMatch[1];
        if (modulePath && this.isInternalPath(modulePath)) {
          const resolved = this.resolveModulePath(filePath, modulePath, projectRoot, fileSet);
          if (resolved) {
            this.addEdge(filePath, resolved, 'require');
          }
        }
      }

      // dynamic import()
      DYNAMIC_IMPORT_PATTERN.lastIndex = 0;
      let dynamicMatch;
      while ((dynamicMatch = DYNAMIC_IMPORT_PATTERN.exec(line)) !== null) {
        const modulePath = dynamicMatch[1];
        if (modulePath && this.isInternalPath(modulePath)) {
          const resolved = this.resolveModulePath(filePath, modulePath, projectRoot, fileSet);
          if (resolved) {
            this.addEdge(filePath, resolved, 'dynamic');
          }
        }
      }

      // Python 相对导入（from .module import x）
      if (isPython) {
        for (const pattern of PY_IMPORT_PATTERNS) {
          pattern.lastIndex = 0;
          const match = pattern.exec(line);
          if (match) {
            const pyModulePath = match[1];
            const resolved = this.resolvePythonImport(filePath, pyModulePath, fileSet);
            if (resolved) {
              this.addEdge(filePath, resolved, 'static');
            }
          }
        }
      }
    }
  }

  /**
   * 添加依赖边
   */
  private addEdge(from: string, to: string, importType: DependencyEdge['importType']): void {
    if (from === to) return;

    // 正向
    if (!this.forwardDeps.has(from)) this.forwardDeps.set(from, new Set());
    this.forwardDeps.get(from)!.add(to);

    // 反向
    if (!this.reverseDeps.has(to)) this.reverseDeps.set(to, new Set());
    this.reverseDeps.get(to)!.add(from);

    this.edges.push({ from, to, importType });
  }

  /**
   * 判断是否为项目内部路径（相对路径 或 tsconfig alias）
   */
  private isInternalPath(modulePath: string): boolean {
    if (modulePath.startsWith('./') || modulePath.startsWith('../')) return true;
    // 优化 #11: 检查是否匹配 tsconfig paths 别名
    for (const alias of this.pathAliases.keys()) {
      if (modulePath.startsWith(alias)) return true;
    }
    return false;
  }

  /**
   * 解析模块路径为项目相对路径（优化 #11: 支持 tsconfig paths）
   */
  private resolveModulePath(
    fromFile: string,
    modulePath: string,
    _projectRoot: string,
    fileSet: Set<string>
  ): string | null {
    let resolvedBase: string;

    if (modulePath.startsWith('./') || modulePath.startsWith('../')) {
      // 相对路径
      const fromDir = path.dirname(fromFile);
      resolvedBase = path.normalize(path.join(fromDir, modulePath));
    } else {
      // 优化 #11: 尝试 tsconfig paths 别名解析
      let matched = false;
      resolvedBase = modulePath;
      for (const [alias, target] of this.pathAliases.entries()) {
        if (modulePath.startsWith(alias)) {
          resolvedBase = path.normalize(modulePath.replace(alias, target));
          matched = true;
          break;
        }
      }
      if (!matched) return null;
    }

    // 尝试多种扩展名（支持多语言）
    const extensions = [
      '', '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs',
      '/index.ts', '/index.js',
      '.py', '__init__.py',
      '.go', '.java', '.rs',
      '.c', '.h', '.cpp', '.cc', '.hpp',
      '.cs', '.php', '.rb', '.swift', '.kt',
    ];
    for (const ext of extensions) {
      const candidate = resolvedBase + ext;
      if (fileSet.has(candidate)) return candidate;
    }

    return null;
  }

  /**
   * 解析 Python 相对导入路径
   * from .module → 同级目录的 module.py
   * from ..module → 父级目录的 module.py
   */
  private resolvePythonImport(
    fromFile: string,
    pyModulePath: string,
    fileSet: Set<string>
  ): string | null {
    const fromDir = path.dirname(fromFile);
    // 计算相对层级：每个前导 '.' 表示上一级
    let dotCount = 0;
    while (dotCount < pyModulePath.length && pyModulePath[dotCount] === '.') {
      dotCount++;
    }
    const moduleName = pyModulePath.substring(dotCount);

    // 构建相对路径
    let targetDir = fromDir;
    for (let i = 1; i < dotCount; i++) {
      targetDir = path.dirname(targetDir);
    }

    if (moduleName) {
      // from .module import x → module.py 或 module/__init__.py
      const asFile = path.normalize(path.join(targetDir, moduleName.replace(/\./g, '/') + '.py'));
      if (fileSet.has(asFile)) return asFile;
      const asPackage = path.normalize(path.join(targetDir, moduleName.replace(/\./g, '/'), '__init__.py'));
      if (fileSet.has(asPackage)) return asPackage;
    } else {
      // from . import x → __init__.py
      const initFile = path.normalize(path.join(targetDir, '__init__.py'));
      if (fileSet.has(initFile)) return initFile;
    }

    return null;
  }

  /**
   * 优化 #11: 从 tsconfig.json 加载 paths 别名
   */
  private loadTsconfigPaths(projectRoot: string): void {
    this.pathAliases.clear();
    try {
      const tsconfigPath = path.join(projectRoot, 'tsconfig.json');
      if (!fs.existsSync(tsconfigPath)) return;

      const raw = fs.readFileSync(tsconfigPath, 'utf-8');
      // 移除 JSON 中的注释（简化处理）
      const cleaned = raw.replace(/\/\/.*$/gm, '').replace(/\/\*[\s\S]*?\*\//g, '');
      const tsconfig = JSON.parse(cleaned);

      const paths = tsconfig?.compilerOptions?.paths;
      const baseUrl = tsconfig?.compilerOptions?.baseUrl || '.';
      if (!paths || typeof paths !== 'object') return;

      for (const [alias, targets] of Object.entries(paths)) {
        if (!Array.isArray(targets) || targets.length === 0) continue;
        // 将 "@/*" → "src/*" 转换为 "@/" → "src/"
        const aliasPrefix = alias.replace(/\*$/, '');
        const targetPrefix = (targets[0] as string).replace(/\*$/, '');
        const resolvedTarget = path.normalize(path.join(baseUrl, targetPrefix));
        this.pathAliases.set(aliasPrefix, resolvedTarget);
      }

      if (this.pathAliases.size > 0) {
        logger.info('依赖图谱.加载 tsconfig paths', {
          aliases: this.pathAliases.size,
        }, LogCategory.SESSION);
      }
    } catch {
      // tsconfig 解析失败，忽略
    }
  }

  /**
   * 计算所有文件的中心度（优化 #6: PageRank 迭代算法）
   * 迭代次数固定 20 轮，阻尼因子 0.85
   */
  private computeCentrality(): void {
    this.centralityCache.clear();
    const allFiles = new Set([...this.forwardDeps.keys(), ...this.reverseDeps.keys()]);
    if (allFiles.size === 0) return;

    const N = allFiles.size;
    const damping = 0.85;
    const iterations = 20;
    const initialScore = 1 / N;

    // 初始化所有文件的 PageRank 值
    let scores = new Map<string, number>();
    for (const file of allFiles) {
      scores.set(file, initialScore);
    }

    // PageRank 迭代
    for (let iter = 0; iter < iterations; iter++) {
      const newScores = new Map<string, number>();
      for (const file of allFiles) {
        newScores.set(file, (1 - damping) / N);
      }

      for (const file of allFiles) {
        const outLinks = this.forwardDeps.get(file);
        const outDegree = outLinks?.size || 0;
        if (outDegree === 0) {
          // 无出链的文件将分数均匀分配给所有文件（避免 rank sink）
          const share = (scores.get(file) || 0) * damping / N;
          for (const f of allFiles) {
            newScores.set(f, (newScores.get(f) || 0) + share);
          }
        } else {
          const share = (scores.get(file) || 0) * damping / outDegree;
          for (const target of outLinks!) {
            if (allFiles.has(target)) {
              newScores.set(target, (newScores.get(target) || 0) + share);
            }
          }
        }
      }
      scores = newScores;
    }

    // 归一化到 [0, 1]
    let maxScore = 0;
    for (const score of scores.values()) {
      if (score > maxScore) maxScore = score;
    }
    for (const [file, score] of scores.entries()) {
      this.centralityCache.set(file, maxScore > 0 ? score / maxScore : 0);
    }
  }
}