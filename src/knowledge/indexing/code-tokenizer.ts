/**
 * CodeTokenizer — 代码分词器
 *
 * 将代码文本分解为可索引的 token，支持：
 * - camelCase / PascalCase / snake_case / UPPER_CASE 标识符拆分
 * - 中文 bigram 分词
 * - 停用词过滤
 * - 代码上下文识别（definition / usage / comment / import / string）
 */

// ============================================================================
// 类型定义
// ============================================================================

/** Token 在代码中的上下文类型 */
export type TokenContext = 'definition' | 'usage' | 'comment' | 'string' | 'import';

/** 带位置信息的 token */
export interface TokenWithPosition {
  /** 归一化后的 token（小写） */
  token: string;
  /** 所在行号（0-based） */
  line: number;
  /** 所在列号（0-based） */
  column: number;
  /** 代码上下文 */
  context: TokenContext;
}

/** 文件分词结果 */
export interface FileTokenResult {
  /** 文件相对路径 */
  filePath: string;
  /** 所有 token 列表 */
  tokens: TokenWithPosition[];
  /** token 频率统计（token → 出现次数） */
  frequencies: Map<string, number>;
  /** token 总数 */
  totalTokens: number;
}

// ============================================================================
// 停用词
// ============================================================================

/** JS/TS 关键字 — 无语义价值，过滤掉 */
const CODE_STOP_WORDS = new Set([
  'const', 'let', 'var', 'function', 'return', 'if', 'else',
  'for', 'while', 'do', 'switch', 'case', 'break', 'continue',
  'new', 'delete', 'typeof', 'instanceof', 'void', 'null',
  'undefined', 'true', 'false', 'try', 'catch', 'finally', 'throw',
  'yield', 'static', 'private', 'protected', 'public', 'readonly',
  'declare', 'module', 'require', 'from', 'as', 'default', 'super',
]);

/** 通用英语停用词 */
const ENGLISH_STOP_WORDS = new Set([
  'the', 'a', 'an', 'is', 'are', 'was', 'were', 'be', 'been',
  'being', 'have', 'has', 'had', 'do', 'does', 'did', 'will',
  'would', 'could', 'should', 'may', 'might', 'shall', 'can',
  'of', 'in', 'to', 'for', 'with', 'on', 'at', 'by',
  'this', 'that', 'these', 'those', 'it', 'its', 'not', 'no',
  'or', 'and', 'but', 'so', 'then', 'than', 'also', 'just',
]);

/** 有结构意义的关键字 — 保留不过滤 */
const STRUCTURAL_KEYWORDS = new Set([
  'import', 'export', 'async', 'await', 'interface', 'type',
  'class', 'enum', 'extends', 'implements', 'abstract',
]);

// ============================================================================
// 正则表达式
// ============================================================================

/** camelCase / PascalCase 拆分：在小写→大写 或 连续大写→大写小写边界处拆分 */
const CAMEL_SPLIT_RE = /([a-z])([A-Z])|([A-Z]+)([A-Z][a-z])/g;

/** 中文字符检测 */
const CHINESE_CHAR_RE = /[\u4e00-\u9fff]/;

/** 纯数字 */
const PURE_NUMBER_RE = /^[0-9]+$/;

/** 合法标识符字符 */
const IDENTIFIER_RE = /^[a-zA-Z_$][a-zA-Z0-9_$]*$/;

// ============================================================================
// CodeTokenizer 类
// ============================================================================

export class CodeTokenizer {
  /**
   * 对文件内容进行分词
   */
  tokenizeFile(filePath: string, content: string): FileTokenResult {
    const tokens: TokenWithPosition[] = [];
    const frequencies = new Map<string, number>();
    const lines = content.split('\n');
    let inBlockComment = false; // 优化 #10: 多行注释状态追踪

    for (let lineIdx = 0; lineIdx < lines.length; lineIdx++) {
      const line = lines[lineIdx];
      const { context, blockCommentState } = this.detectLineContextWithState(line, lineIdx, lines, inBlockComment);
      inBlockComment = blockCommentState;
      const lineTokens = this.tokenizeLine(line, lineIdx, context);

      for (const token of lineTokens) {
        tokens.push(token);
        frequencies.set(token.token, (frequencies.get(token.token) || 0) + 1);
      }
    }

    return {
      filePath,
      tokens,
      frequencies,
      totalTokens: tokens.length,
    };
  }

  /**
   * 对查询文本进行分词（不需要位置信息）
   */
  tokenizeQuery(query: string): string[] {
    const tokens = new Set<string>();

    // 1. 提取中文 token
    const chineseTokens = this.extractChineseTokens(query);
    for (const t of chineseTokens) tokens.add(t);

    // 2. 提取英文标识符并拆分
    const words = query.match(/[a-zA-Z_$][a-zA-Z0-9_$]*/g) || [];
    for (const word of words) {
      const subTokens = this.splitIdentifier(word);
      for (const t of subTokens) {
        if (this.isValidToken(t)) tokens.add(t);
      }
    }

    return Array.from(tokens);
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  /**
   * 检测行的代码上下文类型（优化 #10: 支持多行注释状态追踪）
   */
  private detectLineContextWithState(
    line: string, _lineIdx: number, _lines: string[], inBlockComment: boolean
  ): { context: TokenContext; blockCommentState: boolean } {
    const trimmed = line.trim();

    // 处理多行注释状态
    if (inBlockComment) {
      if (trimmed.includes('*/')) {
        return { context: 'comment', blockCommentState: false };
      }
      return { context: 'comment', blockCommentState: true };
    }

    // 检测多行注释开始（仅当 /* 出现在行首时整行标记为注释）
    if (trimmed.startsWith('/*') && !trimmed.includes('*/')) {
      return { context: 'comment', blockCommentState: true };
    }
    // 单行内完成的块注释 /* ... */（仅行首开始）
    if (trimmed.startsWith('/*') && trimmed.includes('*/')) {
      return { context: 'comment', blockCommentState: false };
    }
    // 行中间出现 /* 且未闭合 → 当前行保持原始 context，但后续行进入块注释状态
    if (trimmed.includes('/*') && !trimmed.includes('*/')) {
      // 不改变当前行的 context（下方会继续匹配 import/definition/usage），
      // 但设置 blockCommentState = true，使后续行进入注释状态
      const innerContext = this.detectInlineContext(trimmed);
      return { context: innerContext, blockCommentState: true };
    }

    // import 语句
    if (trimmed.startsWith('import ') || trimmed.startsWith('import{')) {
      return { context: 'import', blockCommentState: false };
    }

    // 单行注释
    if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/**')) {
      return { context: 'comment', blockCommentState: false };
    }

    // 定义：export / class / interface / type / enum / function 声明
    if (/^(export\s+)?(default\s+)?(class|interface|type|enum|function|const|let|var)\s/.test(trimmed)) {
      return { context: 'definition', blockCommentState: false };
    }

    // 方法定义：  methodName( 或 async methodName(
    if (/^\s*(async\s+)?[a-zA-Z_$][a-zA-Z0-9_$]*\s*\(/.test(trimmed) && !trimmed.includes('=')) {
      return { context: 'definition', blockCommentState: false };
    }

    return { context: 'usage', blockCommentState: false };
  }

  /**
   * 检测行中间出现 /* 时，当前行的真实 context
   * （去除 /* 之后的注释部分，判断代码部分的 context）
   */
  private detectInlineContext(trimmed: string): TokenContext {
    // import 语句
    if (trimmed.startsWith('import ') || trimmed.startsWith('import{')) {
      return 'import';
    }
    // 定义语句
    if (/^(export\s+)?(default\s+)?(class|interface|type|enum|function|const|let|var)\s/.test(trimmed)) {
      return 'definition';
    }
    return 'usage';
  }

  /**
   * 对单行进行分词
   */
  private tokenizeLine(line: string, lineIdx: number, context: TokenContext): TokenWithPosition[] {
    const result: TokenWithPosition[] = [];

    // 1. 提取中文 token
    if (CHINESE_CHAR_RE.test(line)) {
      const chineseTokens = this.extractChineseTokens(line);
      for (const token of chineseTokens) {
        const col = line.indexOf(token);
        result.push({ token, line: lineIdx, column: col >= 0 ? col : 0, context });
      }
    }

    // 2. 提取英文标识符
    const identifierMatches = line.matchAll(/[a-zA-Z_$][a-zA-Z0-9_$]*/g);
    for (const match of identifierMatches) {
      const word = match[0];
      const col = match.index ?? 0;
      const subTokens = this.splitIdentifier(word);

      for (const token of subTokens) {
        if (this.isValidToken(token)) {
          result.push({ token, line: lineIdx, column: col, context });
        }
      }
    }

    return result;
  }

  /**
   * 拆分标识符为子 token
   *
   * getProjectContext → ["getprojectcontext", "get", "project", "context"]
   * HTMLParser → ["htmlparser", "html", "parser"]
   * snake_case → ["snake_case", "snake", "case"]
   * user-service → ["user-service", "user", "service"]  (优化 #9: kebab-case)
   */
  splitIdentifier(identifier: string): string[] {
    if (!identifier || identifier.length < 2) return [];

    const tokens = new Set<string>();
    const lower = identifier.toLowerCase();
    tokens.add(lower); // 始终保留完整原词（小写）

    // snake_case 或 kebab-case 拆分（优化 #9）
    if (identifier.includes('_') || identifier.includes('-')) {
      const parts = identifier.split(/[_-]+/).filter(p => p.length >= 2);
      for (const part of parts) {
        tokens.add(part.toLowerCase());
        // 对每个部分再做 camelCase 拆分
        const camelParts = this.splitCamelCase(part);
        for (const cp of camelParts) tokens.add(cp.toLowerCase());
      }
      return Array.from(tokens);
    }

    // camelCase / PascalCase 拆分
    const camelParts = this.splitCamelCase(identifier);
    for (const part of camelParts) {
      if (part.length >= 2) tokens.add(part.toLowerCase());
    }

    return Array.from(tokens);
  }

  /**
   * camelCase 拆分
   * getProjectContext → ["get", "Project", "Context"]
   * HTMLParser → ["HTML", "Parser"]
   */
  private splitCamelCase(str: string): string[] {
    // 在 小写→大写 和 连续大写→大写小写 边界处插入分隔符
    const split = str.replace(CAMEL_SPLIT_RE, '$1$3\0$2$4');
    return split.split('\0').filter(s => s.length > 0);
  }

  /**
   * 提取中文 token（bigram + trigram + 完整中文段）
   */
  private extractChineseTokens(text: string): string[] {
    const tokens: string[] = [];

    // 提取连续中文段
    const segments = text.match(/[\u4e00-\u9fff]+/g) || [];

    for (const seg of segments) {
      // 保留完整段（长度 >= 2）
      if (seg.length >= 2) tokens.push(seg);

      // bigram
      for (let i = 0; i < seg.length - 1; i++) {
        tokens.push(seg.substring(i, i + 2));
      }

      // trigram（如果段够长）
      for (let i = 0; i < seg.length - 2; i++) {
        tokens.push(seg.substring(i, i + 3));
      }
    }

    return tokens;
  }

  /**
   * 检查 token 是否有效（非停用词、非纯数字、长度 >= 2）
   */
  private isValidToken(token: string): boolean {
    if (token.length < 2) return false;
    if (PURE_NUMBER_RE.test(token)) return false;

    // 中文 token 不检查停用词
    if (CHINESE_CHAR_RE.test(token)) return true;

    const lower = token.toLowerCase();

    // 有结构意义的关键字保留
    if (STRUCTURAL_KEYWORDS.has(lower)) return true;

    // 停用词过滤
    if (CODE_STOP_WORDS.has(lower)) return false;
    if (ENGLISH_STOP_WORDS.has(lower)) return false;

    return true;
  }
}