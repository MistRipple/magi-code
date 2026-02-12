/**
 * SymbolIndex — 代码符号索引
 *
 * 通过正则提取代码中的结构化符号（函数、类、接口、类型、变量导出），
 * 提供比全文检索更精确的代码结构查询能力。
 *
 * 不使用完整 AST（避免依赖 TypeScript compiler API），
 * 而是通过精心设计的正则实现 80/20 法则的最佳平衡。
 */

import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../../logging';
import { MinHeap } from '../utils/min-heap';

// ============================================================================
// 类型定义
// ============================================================================

/** 符号类型 */
export type SymbolKind = 'function' | 'class' | 'interface' | 'type' | 'enum' | 'variable' | 'method';

/** 符号条目 */
export interface SymbolEntry {
  /** 符号名称 */
  name: string;
  /** 符号类型 */
  kind: SymbolKind;
  /** 文件相对路径 */
  filePath: string;
  /** 定义行号（0-based） */
  line: number;
  /** 代码块结束行号（0-based，用于代码块级索引） */
  endLine?: number;
  /** 是否为导出符号 */
  isExported: boolean;
  /** 所属容器（如方法属于类） */
  container?: string;
  /** 签名摘要（简化的参数类型信息） */
  signature?: string;
}

/** 符号搜索命中 */
export interface SymbolSearchHit {
  symbol: SymbolEntry;
  /** 匹配得分（0-1） */
  score: number;
  /** 匹配方式：exact=完全匹配, prefix=前缀, contains=包含, fuzzy=模糊 */
  matchType: 'exact' | 'prefix' | 'contains' | 'fuzzy';
}

/** 符号索引序列化快照 */
export interface SymbolIndexSnapshot {
  symbols: Array<[string, SymbolEntry[]]>;
  fileSymbols: Array<[string, string[]]>;
}

// ============================================================================
// 符号提取正则
// ============================================================================

/** TS/JS 符号提取模式 */
const TS_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number; signatureGroup?: number }> = [
  // export function funcName(...)
  { kind: 'function', pattern: /^(export\s+)?(?:async\s+)?function\s+([a-zA-Z_$][a-zA-Z0-9_$]*)\s*(<[^>]*>)?\s*\(([^)]*)\)/gm, nameGroup: 2 },
  // export const funcName = (...) => / function(...)
  { kind: 'function', pattern: /^(export\s+)?(?:const|let|var)\s+([a-zA-Z_$][a-zA-Z0-9_$]*)\s*=\s*(?:async\s+)?(?:\([^)]*\)|[a-zA-Z_$][a-zA-Z0-9_$]*)\s*=>/gm, nameGroup: 2 },
  // export class ClassName
  { kind: 'class', pattern: /^(export\s+)?(?:abstract\s+)?class\s+([a-zA-Z_$][a-zA-Z0-9_$]*)(?:\s+extends\s+[a-zA-Z_$][a-zA-Z0-9_$.<>,\s]*)?(?:\s+implements\s+[a-zA-Z_$][a-zA-Z0-9_$.<>,\s]*)?\s*\{/gm, nameGroup: 2 },
  // export interface InterfaceName
  { kind: 'interface', pattern: /^(export\s+)?interface\s+([a-zA-Z_$][a-zA-Z0-9_$]*)(?:\s+extends\s+[a-zA-Z_$][a-zA-Z0-9_$.<>,\s]*)?\s*\{/gm, nameGroup: 2 },
  // export type TypeName =
  { kind: 'type', pattern: /^(export\s+)?type\s+([a-zA-Z_$][a-zA-Z0-9_$]*)(?:<[^>]*>)?\s*=/gm, nameGroup: 2 },
  // export enum EnumName
  { kind: 'enum', pattern: /^(export\s+)?(?:const\s+)?enum\s+([a-zA-Z_$][a-zA-Z0-9_$]*)\s*\{/gm, nameGroup: 2 },
  // export const/let/var VARIABLE_NAME (大写或 PascalCase，排除函数)
  { kind: 'variable', pattern: /^(export\s+)(?:const|let|var)\s+([A-Z][a-zA-Z0-9_$]*)\s*(?::[^=]+)?\s*=/gm, nameGroup: 2 },
];

/** Python 符号提取模式 */
const PY_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // def func_name(...) / async def func_name(...)
  { kind: 'function', pattern: /^(?:async\s+)?def\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*\(/gm, nameGroup: 1 },
  // class ClassName(...): / class ClassName:
  { kind: 'class', pattern: /^class\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*[:(]/gm, nameGroup: 1 },
];

/** Go 符号提取模式 */
const GO_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // func FuncName(...) / func (r *Recv) MethodName(...)
  { kind: 'function', pattern: /^func\s+(?:\([^)]+\)\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*[\[(]/gm, nameGroup: 1 },
  // type TypeName struct/interface {
  { kind: 'type', pattern: /^type\s+([A-Za-z_][A-Za-z0-9_]*)\s+(?:struct|interface)\s*\{/gm, nameGroup: 1 },
];

/** Java 符号提取模式 */
const JAVA_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // [public|private|protected] [abstract|final] class ClassName
  { kind: 'class', pattern: /^(?:public\s+|private\s+|protected\s+)?(?:abstract\s+|final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // [public] interface InterfaceName
  { kind: 'interface', pattern: /^(?:public\s+|private\s+|protected\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // enum EnumName
  { kind: 'enum', pattern: /^(?:public\s+|private\s+|protected\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
];

/** Rust 符号提取模式 */
const RUST_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // [pub] [async] fn func_name(...)
  { kind: 'function', pattern: /^(?:pub\s+(?:\(crate\)\s+)?)?(?:async\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)/gm, nameGroup: 1 },
  // [pub] struct StructName
  { kind: 'class', pattern: /^(?:pub\s+(?:\(crate\)\s+)?)?struct\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // [pub] trait TraitName
  { kind: 'interface', pattern: /^(?:pub\s+(?:\(crate\)\s+)?)?trait\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // [pub] enum EnumName
  { kind: 'enum', pattern: /^(?:pub\s+(?:\(crate\)\s+)?)?enum\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // [pub] type TypeAlias =
  { kind: 'type', pattern: /^(?:pub\s+(?:\(crate\)\s+)?)?type\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
];

/** C/C++ 符号提取模式 */
const C_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // 函数定义: 返回类型 func_name(  （行首，排除缩进行即类方法）
  { kind: 'function', pattern: /^(?:static\s+|inline\s+|extern\s+|virtual\s+)*(?:const\s+)?[A-Za-z_][A-Za-z0-9_*&\s:<>,]*\s+\*?([A-Za-z_][A-Za-z0-9_]*)\s*\(/gm, nameGroup: 1 },
  // struct StructName {
  { kind: 'class', pattern: /^(?:typedef\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{/gm, nameGroup: 1 },
  // class ClassName
  { kind: 'class', pattern: /^class\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s*:\s*(?:public|private|protected))?\s*/gm, nameGroup: 1 },
  // typedef ... TypeName;  (简化：捕获最后一个标识符)
  { kind: 'type', pattern: /^typedef\s+.*\s+([A-Za-z_][A-Za-z0-9_]*)\s*;/gm, nameGroup: 1 },
  // namespace NamespaceName {
  { kind: 'type', pattern: /^namespace\s+([A-Za-z_][A-Za-z0-9_:]*)\s*\{/gm, nameGroup: 1 },
  // enum [class] EnumName
  { kind: 'enum', pattern: /^enum\s+(?:class\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\{/gm, nameGroup: 1 },
];

/** C# 符号提取模式 */
const CSHARP_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // [public] [abstract|sealed|static|partial] class ClassName
  { kind: 'class', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:abstract\s+|sealed\s+|static\s+|partial\s+)*class\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // interface IName
  { kind: 'interface', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:partial\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // enum EnumName
  { kind: 'enum', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // struct StructName
  { kind: 'class', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:readonly\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // namespace NamespaceName
  { kind: 'type', pattern: /^namespace\s+([A-Za-z_][A-Za-z0-9_.]*)/gm, nameGroup: 1 },
];

/** PHP 符号提取模式 */
const PHP_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // function funcName(
  { kind: 'function', pattern: /^(?:public\s+|private\s+|protected\s+)?(?:static\s+)?function\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*\(/gm, nameGroup: 1 },
  // class ClassName
  { kind: 'class', pattern: /^(?:abstract\s+|final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // interface InterfaceName
  { kind: 'interface', pattern: /^interface\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // trait TraitName
  { kind: 'interface', pattern: /^trait\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
];

/** Ruby 符号提取模式 */
const RUBY_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // def method_name / def self.method_name
  { kind: 'function', pattern: /^\s*def\s+(?:self\.)?([a-zA-Z_][a-zA-Z0-9_!?=]*)/gm, nameGroup: 1 },
  // class ClassName
  { kind: 'class', pattern: /^\s*class\s+([A-Z][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // module ModuleName
  { kind: 'type', pattern: /^\s*module\s+([A-Z][A-Za-z0-9_]*)/gm, nameGroup: 1 },
];

/** Swift 符号提取模式 */
const SWIFT_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // [public|open] func funcName(
  { kind: 'function', pattern: /^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?(?:static\s+|class\s+)?func\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*[\(<]/gm, nameGroup: 1 },
  // class ClassName
  { kind: 'class', pattern: /^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?(?:final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // struct StructName
  { kind: 'class', pattern: /^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // protocol ProtocolName
  { kind: 'interface', pattern: /^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?protocol\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // enum EnumName
  { kind: 'enum', pattern: /^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
];

/** Kotlin 符号提取模式 */
const KOTLIN_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // [public] fun funcName(
  { kind: 'function', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:suspend\s+|inline\s+|override\s+)*fun\s+(?:<[^>]+>\s+)?([a-zA-Z_][a-zA-Z0-9_]*)\s*[\(<]/gm, nameGroup: 1 },
  // [data|sealed|abstract|open] class ClassName
  { kind: 'class', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:data\s+|sealed\s+|abstract\s+|open\s+|inner\s+)*class\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // interface InterfaceName
  { kind: 'interface', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:sealed\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // object ObjectName
  { kind: 'class', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?object\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // enum class EnumName
  { kind: 'enum', pattern: /^(?:public\s+|private\s+|protected\s+|internal\s+)?enum\s+class\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
];

/** Objective-C 符号提取模式 */
const OBJC_SYMBOL_PATTERNS: Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }> = [
  // @interface ClassName / @interface ClassName (CategoryName)
  { kind: 'class', pattern: /^@interface\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // @implementation ClassName
  { kind: 'class', pattern: /^@implementation\s+([A-Za-z_][A-Za-z0-9_]*)/gm, nameGroup: 1 },
  // @protocol ProtocolName
  { kind: 'interface', pattern: /^@protocol\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<|$)/gm, nameGroup: 1 },
  // - (ReturnType)methodName 或 + (ReturnType)methodName（实例/类方法）
  { kind: 'method', pattern: /^[-+]\s*\([^)]+\)\s*([a-zA-Z_][a-zA-Z0-9_]*)/gm, nameGroup: 1 },
  // C-style function（ObjC 文件中也可能有纯 C 函数）
  { kind: 'function', pattern: /^(?:static\s+|inline\s+|extern\s+)*[A-Za-z_][A-Za-z0-9_*\s]+\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*\(/gm, nameGroup: 1 },
];

/** 根据文件扩展名选择符号模式 */
const LANG_PATTERNS: Record<string, Array<{ kind: SymbolKind; pattern: RegExp; nameGroup: number }>> = {
  '.ts': TS_SYMBOL_PATTERNS,
  '.tsx': TS_SYMBOL_PATTERNS,
  '.js': TS_SYMBOL_PATTERNS,
  '.jsx': TS_SYMBOL_PATTERNS,
  '.mjs': TS_SYMBOL_PATTERNS,
  '.cjs': TS_SYMBOL_PATTERNS,
  '.py': PY_SYMBOL_PATTERNS,
  '.go': GO_SYMBOL_PATTERNS,
  '.java': JAVA_SYMBOL_PATTERNS,
  '.rs': RUST_SYMBOL_PATTERNS,
  '.c': C_SYMBOL_PATTERNS,
  '.h': C_SYMBOL_PATTERNS,
  '.cpp': C_SYMBOL_PATTERNS,
  '.cc': C_SYMBOL_PATTERNS,
  '.cxx': C_SYMBOL_PATTERNS,
  '.hpp': C_SYMBOL_PATTERNS,
  '.hh': C_SYMBOL_PATTERNS,
  '.cs': CSHARP_SYMBOL_PATTERNS,
  '.php': PHP_SYMBOL_PATTERNS,
  '.rb': RUBY_SYMBOL_PATTERNS,
  '.swift': SWIFT_SYMBOL_PATTERNS,
  '.kt': KOTLIN_SYMBOL_PATTERNS,
  '.kts': KOTLIN_SYMBOL_PATTERNS,
  '.m': OBJC_SYMBOL_PATTERNS,
  '.mm': OBJC_SYMBOL_PATTERNS,
};

/** TS/JS 方法模式（类内部） */
const METHOD_PATTERN = /^\s+(?:(?:public|private|protected|static|async|readonly|abstract|override)\s+)*([a-zA-Z_$][a-zA-Z0-9_$]*)\s*(?:<[^>]*>)?\s*\(([^)]*)\)/gm;

/** Java 方法模式（类内部）: [修饰符] [泛型] 返回类型 methodName(...) */
const JAVA_METHOD_PATTERN = /^\s+(?:(?:public|private|protected|static|final|abstract|synchronized|native)\s+)*(?:<[^>]+>\s+)?[A-Za-z_][A-Za-z0-9_<>,\[\]\s.]*\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*\(/gm;

/** 优化 #14: re-export 模式 — export { x, y } from './module' */
const REEXPORT_PATTERN = /^export\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]/gm;

// ============================================================================
// SymbolIndex 类
// ============================================================================

export class SymbolIndex {
  /** 符号名称 → 符号条目列表（多个文件可能有同名符号） */
  private symbols = new Map<string, SymbolEntry[]>();
  /** 文件 → 其中的符号名称列表 */
  private fileSymbols = new Map<string, string[]>();
  private _isReady = false;

  get isReady(): boolean {
    return this._isReady;
  }

  /**
   * 从文件列表构建符号索引
   */
  async buildFromFiles(
    projectRoot: string,
    files: Array<{ path: string; type: 'source' | 'config' | 'doc' | 'test' }>
  ): Promise<void> {
    this.clear();
    const startTime = Date.now();
    let processedCount = 0;

    // 只索引源码和测试文件（排除 config/doc）
    const sourceFiles = files.filter(f => f.type === 'source' || f.type === 'test');

    const BATCH_SIZE = 50; // 每批处理 50 个文件后让出事件循环
    for (let i = 0; i < sourceFiles.length; i++) {
      const file = sourceFiles[i];
      try {
        const fullPath = path.join(projectRoot, file.path);
        if (!fs.existsSync(fullPath)) continue;

        const stat = fs.statSync(fullPath);
        if (stat.size > 500 * 1024) continue; // 跳过大文件

        const content = fs.readFileSync(fullPath, 'utf-8');
        this.extractSymbols(file.path, content);
        processedCount++;
      } catch {
        // 跳过无法读取的文件
      }

      // 每处理 BATCH_SIZE 个文件后让出事件循环，避免阻塞 UI
      if ((i + 1) % BATCH_SIZE === 0) {
        await new Promise<void>(resolve => setImmediate(resolve));
      }
    }

    this._isReady = true;
    const elapsed = Date.now() - startTime;

    logger.info('符号索引.构建完成', {
      files: processedCount,
      symbols: this.symbols.size,
      elapsed: `${elapsed}ms`,
    }, LogCategory.SESSION);
  }

  /**
   * 搜索符号（单个查询词）
   */
  search(query: string, maxResults = 20): SymbolSearchHit[] {
    if (!this._isReady || !query.trim()) return [];

    const queryLower = query.toLowerCase();
    const hits: SymbolSearchHit[] = [];

    for (const [name, entries] of this.symbols.entries()) {
      const nameLower = name.toLowerCase();
      let matchType: SymbolSearchHit['matchType'] | null = null;
      let score = 0;

      if (nameLower === queryLower) {
        matchType = 'exact';
        score = 1.0;
      } else if (nameLower.startsWith(queryLower)) {
        matchType = 'prefix';
        score = 0.8;
      } else if (nameLower.includes(queryLower)) {
        matchType = 'contains';
        score = 0.5;
      } else if (this.fuzzyMatch(queryLower, nameLower)) {
        matchType = 'fuzzy';
        score = 0.3;
      }

      if (matchType) {
        for (const entry of entries) {
          const exportBonus = entry.isExported ? 0.15 : 0;
          const kindWeight = this.getKindWeight(entry.kind);
          hits.push({
            symbol: entry,
            score: Math.min(1.0, score + exportBonus + kindWeight * 0.1),
            matchType,
          });
        }
      }
    }

    return hits
      .sort((a, b) => b.score - a.score)
      .slice(0, maxResults);
  }

  /**
   * 多 token 联合搜索（优化 #12 → #16: 单次遍历优化）
   *
   * 将原来的 N+1 次全表扫描合并为 1 次遍历：
   * - 对每个符号名，一次性检查所有 queryTerms（exact > prefix > contains > fuzzy）
   * - fuzzyMatch 仅对 originalQuery 执行（扩展 token 的 fuzzy 匹配价值低、代价高）
   * - 命中 exact 时提前 break（无需检查剩余 terms）
   *
   * 复杂度: 从 O(S × (N+1)) 降为 O(S × 1)（单次 Map 遍历 + 内循环短路）
   */
  searchMulti(tokens: string[], maxResults = 20, originalQuery?: string): SymbolSearchHit[] {
    if (!this._isReady || tokens.length === 0) return [];

    // 预处理查询词：lowercase + 去重 + 过滤短词
    const queryTerms: string[] = [];
    const seen = new Set<string>();
    let originalQueryLower: string | null = null;

    if (originalQuery && originalQuery.trim().length >= 2) {
      originalQueryLower = originalQuery.trim().toLowerCase();
      queryTerms.push(originalQueryLower);
      seen.add(originalQueryLower);
    }

    for (const token of tokens) {
      if (!token || token.length < 2) continue;
      const tl = token.toLowerCase();
      if (seen.has(tl)) continue;
      seen.add(tl);
      queryTerms.push(tl);
    }

    if (queryTerms.length === 0) return [];

    // 单次遍历所有符号
    const hitMap = new Map<string, SymbolSearchHit>();

    for (const [name, entries] of this.symbols.entries()) {
      const nameLower = name.toLowerCase();

      let bestMatchType: SymbolSearchHit['matchType'] | null = null;
      let bestScore = 0;

      // 对当前符号名，检查所有 queryTerms
      for (const term of queryTerms) {
        let matchType: SymbolSearchHit['matchType'] | null = null;
        let score = 0;

        if (nameLower === term) {
          matchType = 'exact'; score = 1.0;
        } else if (nameLower.startsWith(term)) {
          matchType = 'prefix'; score = 0.8;
        } else if (nameLower.includes(term)) {
          matchType = 'contains'; score = 0.5;
        }

        if (matchType && score > bestScore) {
          bestScore = score;
          bestMatchType = matchType;
          if (score >= 1.0) break; // exact 命中，无需继续检查
        }
      }

      // fuzzyMatch 仅对 originalQuery 执行（代价较高，限制范围）
      if (!bestMatchType && originalQueryLower && this.fuzzyMatch(originalQueryLower, nameLower)) {
        bestMatchType = 'fuzzy';
        bestScore = 0.3;
      }

      if (bestMatchType) {
        for (const entry of entries) {
          const exportBonus = entry.isExported ? 0.15 : 0;
          const kindWeight = this.getKindWeight(entry.kind);
          const finalScore = Math.min(1.0, bestScore + exportBonus + kindWeight * 0.1);
          const key = `${entry.filePath}:${name}:${entry.line}`;
          const existing = hitMap.get(key);
          if (!existing || finalScore > existing.score) {
            hitMap.set(key, { symbol: entry, score: finalScore, matchType: bestMatchType });
          }
        }
      }
    }

    // 优化 #17: MinHeap Top-K 替换全量 sort+slice
    const heap = new MinHeap<SymbolSearchHit>(maxResults, (a, b) => a.score - b.score);
    for (const hit of hitMap.values()) {
      heap.push(hit);
    }
    return heap.toSortedDescArray();
  }

  /**
   * 按文件路径获取该文件的所有符号
   */
  getSymbolsForFile(filePath: string): SymbolEntry[] {
    const symbolNames = this.fileSymbols.get(filePath) || [];
    const results: SymbolEntry[] = [];
    for (const name of symbolNames) {
      const entries = this.symbols.get(name) || [];
      results.push(...entries.filter(e => e.filePath === filePath));
    }
    return results;
  }

  /**
   * 代码块级索引: 查找包含指定行号的最小代码块符号
   * 当多个符号嵌套时（如类内方法），返回范围最小（最精确）的那个
   * @param filePath 文件相对路径
   * @param line 行号（0-based）
   * @returns 包含该行的最小符号条目，或 null
   */
  getSymbolAtLine(filePath: string, line: number): SymbolEntry | null {
    const symbols = this.getSymbolsForFile(filePath);
    let bestMatch: SymbolEntry | null = null;
    let bestRange = Infinity;

    for (const sym of symbols) {
      const startLine = sym.line;
      const endLine = sym.endLine ?? sym.line;
      if (line >= startLine && line <= endLine) {
        const range = endLine - startLine;
        if (range < bestRange) {
          bestRange = range;
          bestMatch = sym;
        }
      }
    }

    return bestMatch;
  }

  /**
   * 增量更新单个文件
   */
  updateFile(projectRoot: string, filePath: string): void {
    this.removeFile(filePath);
    try {
      const fullPath = path.join(projectRoot, filePath);
      if (!fs.existsSync(fullPath)) return;
      const stat = fs.statSync(fullPath);
      if (stat.size > 500 * 1024) return;
      const content = fs.readFileSync(fullPath, 'utf-8');
      this.extractSymbols(filePath, content);
    } catch {
      // 跳过
    }
  }

  /**
   * 从索引中删除文件
   */
  removeFile(filePath: string): void {
    const symbolNames = this.fileSymbols.get(filePath);
    if (!symbolNames) return;

    for (const name of symbolNames) {
      const entries = this.symbols.get(name);
      if (!entries) continue;
      const filtered = entries.filter(e => e.filePath !== filePath);
      if (filtered.length === 0) {
        this.symbols.delete(name);
      } else {
        this.symbols.set(name, filtered);
      }
    }
    this.fileSymbols.delete(filePath);
  }

  /**
   * 清空索引
   */
  clear(): void {
    this.symbols.clear();
    this.fileSymbols.clear();
    this._isReady = false;
  }

  /**
   * 获取统计信息
   */
  getStats(): { uniqueSymbols: number; totalEntries: number; isReady: boolean } {
    let totalEntries = 0;
    for (const entries of this.symbols.values()) {
      totalEntries += entries.length;
    }
    return {
      uniqueSymbols: this.symbols.size,
      totalEntries,
      isReady: this._isReady,
    };
  }

  // ==========================================================================
  // 序列化 / 反序列化
  // ==========================================================================

  /**
   * 序列化为 JSON 可存储对象
   */
  toJSON(): SymbolIndexSnapshot {
    return {
      symbols: Array.from(this.symbols.entries()),
      fileSymbols: Array.from(this.fileSymbols.entries()),
    };
  }

  /**
   * 从序列化数据恢复索引
   * 对旧快照中缺少 endLine 的条目进行数据迁移
   */
  fromJSON(snapshot: SymbolIndexSnapshot): void {
    this.clear();
    this.symbols = new Map(snapshot.symbols);

    // 数据迁移：旧快照可能没有 endLine 字段，补设为 line（单行范围）
    for (const entries of this.symbols.values()) {
      for (const entry of entries) {
        if (entry.endLine === undefined) {
          entry.endLine = entry.line;
        }
      }
    }

    this.fileSymbols = new Map(snapshot.fileSymbols);
    this._isReady = true;
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  /**
   * 从文件内容中提取符号
   * 优化 #13: 通过花括号深度追踪方法的类归属
   * 优化 #14: 捕获 re-export 模式
   * 代码块级索引: 通过 pendingBlocks 栈追踪符号的 endLine
   */
  private extractSymbols(filePath: string, content: string): void {
    const ext = path.extname(filePath);
    const patterns = LANG_PATTERNS[ext];
    if (!patterns) return; // 不支持的语言，跳过

    const isTS = ['.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs'].includes(ext);
    const isJava = ext === '.java';
    const isCSharp = ext === '.cs';
    const isCpp = ['.cpp', '.cc', '.cxx', '.hpp', '.hh'].includes(ext);
    const hasClassScope = isTS || isJava || isCSharp || isCpp; // 支持类内方法提取的语言
    const lines = content.split('\n');
    const symbolNames: string[] = [];

    // 花括号深度栈追踪类归属 + 符号 endLine（TS/JS + Java + C# + C++）
    let braceDepth = 0;
    const classStack: Array<{ className: string; startDepth: number }> = [];
    const pendingBlocks: Array<{ entry: SymbolEntry; startDepth: number }> = [];
    // 待匹配花括号的符号声明（处理多行参数列表场景）
    let pendingDeclaration: { entry: SymbolEntry; maxLine: number } | null = null;

    for (let lineIdx = 0; lineIdx < lines.length; lineIdx++) {
      const line = lines[lineIdx];

      // 跳过纯注释行
      const trimmed = line.trimStart();
      if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')
          || trimmed.startsWith('#') /* Python 注释 / Rust 属性 */) {
        continue;
      }

      // TS/JS 专有：检测 re-export 模式
      if (isTS) {
        REEXPORT_PATTERN.lastIndex = 0;
        const reexportMatch = REEXPORT_PATTERN.exec(line);
        if (reexportMatch) {
          const exportedNames = reexportMatch[1].split(',').map(s => s.trim().split(/\s+as\s+/).pop()?.trim()).filter(Boolean);
          for (const name of exportedNames) {
            if (name && name.length >= 2) {
              const entry: SymbolEntry = {
                name,
                kind: 'variable',
                filePath,
                line: lineIdx,
                endLine: lineIdx, // re-export 是单行
                isExported: true,
              };
              this.addSymbol(name, entry);
              symbolNames.push(name);
            }
          }
        }
      }

      // 追踪花括号深度（排除字符串内的花括号）
      const strippedLine = this.stripStrings(line);
      const openBraces = (strippedLine.match(/\{/g) || []).length;
      const closeBraces = (strippedLine.match(/\}/g) || []).length;

      // 检测 class 声明 → 压入 classStack（TS/JS + Java）
      if (hasClassScope) {
        for (const sp of patterns) {
          if (sp.kind === 'class') {
            sp.pattern.lastIndex = 0;
            const classMatch = sp.pattern.exec(line);
            if (classMatch) {
              const className = classMatch[sp.nameGroup];
              classStack.push({ className, startDepth: braceDepth });
            }
          }
        }
      }

      // 保存更新前的花括号深度（用于 pendingBlocks 的 startDepth 计算）
      const prevBraceDepth = braceDepth;
      braceDepth += openBraces - closeBraces;

      // 多行参数列表：在 braceDepth 更新后，检查是否有等待匹配 { 的符号声明
      if (pendingDeclaration) {
        if (lineIdx > pendingDeclaration.maxLine) {
          // 超过最大搜索范围仍未找到 { → 视为单行声明（如 .d.ts 重载签名）
          pendingDeclaration.entry.endLine = pendingDeclaration.entry.line;
          pendingDeclaration = null;
        } else if (openBraces > 0) {
          // 找到了 { → 将暂存的符号声明转入 pendingBlocks 追踪 endLine
          pendingBlocks.push({ entry: pendingDeclaration.entry, startDepth: prevBraceDepth });
          pendingDeclaration = null;
        }
      }

      // 当花括号深度回退到类的起始深度，说明类结束
      while (classStack.length > 0 && braceDepth <= classStack[classStack.length - 1].startDepth) {
        classStack.pop();
      }

      // 代码块级索引: 花括号回退时，关闭已完成的符号块
      while (pendingBlocks.length > 0 && braceDepth <= pendingBlocks[pendingBlocks.length - 1].startDepth) {
        const closed = pendingBlocks.pop()!;
        closed.entry.endLine = lineIdx;
      }

      const currentClass = classStack.length > 0 ? classStack[classStack.length - 1].className : undefined;

      // 提取顶层符号
      for (const sp of patterns) {
        sp.pattern.lastIndex = 0;
        const match = sp.pattern.exec(line);
        if (match) {
          const name = match[sp.nameGroup];
          if (!name || name.length < 2) continue;

          // TS/JS 有 export 标记，其他语言通过命名约定判断
          const isExported = isTS
            ? !!match[1]?.includes('export')
            : /^[A-Z]/.test(name); // Go/Java/Rust 大写开头为导出
          const isBlockKind = ['class', 'interface', 'enum', 'function'].includes(sp.kind);
          const hasBlock = openBraces > 0 && isBlockKind;
          const entry: SymbolEntry = {
            name,
            kind: sp.kind,
            filePath,
            line: lineIdx,
            endLine: isBlockKind ? undefined : lineIdx, // 块类型等待花括号确定，非块类型为单行
            isExported,
          };

          if (hasBlock) {
            // 声明行包含 { → 直接追踪 endLine
            pendingBlocks.push({ entry, startDepth: prevBraceDepth });
          } else if (isBlockKind) {
            // 声明行无 { → 可能是多行参数列表，暂存等待后续行的 {
            // 关闭旧的 pendingDeclaration（防止被覆盖导致 endLine 泄漏）
            if (pendingDeclaration) {
              pendingDeclaration.entry.endLine = pendingDeclaration.entry.line;
            }
            pendingDeclaration = { entry, maxLine: lineIdx + 10 };
          }

          this.addSymbol(name, entry);
          symbolNames.push(name);
        }
      }

      // TS/JS + Java + C# + C++：提取类方法
      if (hasClassScope && currentClass && trimmed.length > 0) {
        const methodPattern = (isJava || isCSharp) ? JAVA_METHOD_PATTERN : METHOD_PATTERN;
        methodPattern.lastIndex = 0;
        const methodMatch = methodPattern.exec(line);
        if (methodMatch) {
          const methodName = methodMatch[1];
          if (methodName && methodName.length >= 2 && methodName !== 'constructor'
              && (!(isJava || isCSharp) || methodName !== currentClass) /* Java/C# 排除构造函数 */) {
            const hasMethodBlock = openBraces > 0;
            const isMethodExported = (isJava || isCSharp) ? /^\s*public\s/.test(line) : false;
            const entry: SymbolEntry = {
              name: methodName,
              kind: 'method',
              filePath,
              line: lineIdx,
              endLine: undefined, // 方法始终等待花括号确定 endLine
              isExported: isMethodExported,
              container: currentClass,
            };
            this.addSymbol(methodName, entry);
            symbolNames.push(methodName);

            if (hasMethodBlock) {
              // 方法声明行包含 { → 直接追踪 endLine
              pendingBlocks.push({ entry, startDepth: prevBraceDepth });
            } else {
              // 方法声明行无 { → 暂存等待后续行的 {
              // 关闭旧的 pendingDeclaration（防止被覆盖导致 endLine 泄漏）
              if (pendingDeclaration) {
                pendingDeclaration.entry.endLine = pendingDeclaration.entry.line;
              }
              pendingDeclaration = { entry, maxLine: lineIdx + 10 };
            }
          }
        }
      }
    }

    // 文件结束时，关闭仍未关闭的符号块和待匹配声明
    if (pendingDeclaration) {
      pendingDeclaration.entry.endLine = pendingDeclaration.entry.line;
      pendingDeclaration = null;
    }
    for (const pending of pendingBlocks) {
      if (pending.entry.endLine === undefined) {
        pending.entry.endLine = lines.length - 1;
      }
    }

    if (symbolNames.length > 0) {
      this.fileSymbols.set(filePath, symbolNames);
    }
  }

  /**
   * 添加符号到索引
   */
  private addSymbol(name: string, entry: SymbolEntry): void {
    const existing = this.symbols.get(name) || [];
    existing.push(entry);
    this.symbols.set(name, existing);
  }

  /**
   * 模糊匹配（子序列匹配）
   */
  private fuzzyMatch(query: string, target: string): boolean {
    let qi = 0;
    for (let ti = 0; ti < target.length && qi < query.length; ti++) {
      if (query[qi] === target[ti]) qi++;
    }
    return qi === query.length && query.length >= 3;
  }

  /**
   * 符号类型权重
   */
  private getKindWeight(kind: SymbolKind): number {
    switch (kind) {
      case 'class': return 1.0;
      case 'interface': return 0.9;
      case 'function': return 0.8;
      case 'type': return 0.7;
      case 'enum': return 0.7;
      case 'method': return 0.6;
      case 'variable': return 0.5;
      default: return 0.3;
    }
  }

  /**
   * 优化 #13: 去除行中字符串内容，避免字符串内的花括号干扰深度追踪
   */
  private stripStrings(line: string): string {
    return line
      .replace(/'(?:[^'\\]|\\.)*'/g, '""')
      .replace(/"(?:[^"\\]|\\.)*"/g, '""')
      .replace(/`(?:[^`\\]|\\.)*`/g, '""');
  }
}