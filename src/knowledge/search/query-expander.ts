/**
 * QueryExpander — 查询扩展器
 *
 * 将用户的自然语言查询扩展为多组搜索关键词，提升检索覆盖率。
 *
 * 两种扩展模式：
 * 1. 离线同义词扩展（编程领域常见词映射，零延迟）
 * 2. LLM 在线扩展（利用已有 LLM 客户端，更智能但有延迟）
 *
 * 设计原则：
 * - LLM 不可用时自动降级为离线模式
 * - 扩展结果合并去重
 * - 控制扩展后的关键词数量上限
 */

import { LLMClient } from '../../llm/types';
import { logger, LogCategory } from '../../logging';

// ============================================================================
// 类型定义
// ============================================================================

/** 扩展结果 */
export interface ExpandedQuery {
  /** 原始查询 */
  original: string;
  /** 扩展后的搜索词列表 */
  expandedTokens: string[];
  /** 使用的扩展模式 */
  mode: 'offline' | 'llm' | 'hybrid';
}

// ============================================================================
// 编程领域同义词映射表
// ============================================================================

const SYNONYM_MAP: Record<string, string[]> = {
  // 中文 → 英文常见标识符
  '登录': ['login', 'auth', 'signin', 'authenticate'],
  '注册': ['register', 'signup', 'createAccount'],
  '用户': ['user', 'account', 'profile'],
  '密码': ['password', 'credential', 'secret'],
  '权限': ['permission', 'authorization', 'access', 'role'],
  '配置': ['config', 'configuration', 'settings', 'options'],
  '数据库': ['database', 'db', 'storage', 'repository'],
  '缓存': ['cache', 'memo', 'memoize'],
  '错误': ['error', 'exception', 'fault', 'failure'],
  '日志': ['log', 'logger', 'logging', 'trace'],
  '测试': ['test', 'spec', 'unittest', 'jest'],
  '路由': ['route', 'router', 'routing', 'path'],
  '请求': ['request', 'req', 'fetch', 'http'],
  '响应': ['response', 'res', 'reply'],
  '模型': ['model', 'schema', 'entity'],
  '服务': ['service', 'provider', 'handler'],
  '控制器': ['controller', 'handler', 'endpoint'],
  '中间件': ['middleware', 'interceptor', 'filter'],
  '组件': ['component', 'widget', 'element'],
  '状态': ['state', 'status', 'store'],
  '事件': ['event', 'emitter', 'listener', 'handler'],
  '接口': ['interface', 'api', 'contract', 'protocol'],
  '类型': ['type', 'typedef', 'typing'],
  '工具': ['tool', 'util', 'utility', 'helper'],
  '搜索': ['search', 'find', 'query', 'lookup'],
  '索引': ['index', 'indexing', 'inverted'],
  '解析': ['parse', 'parser', 'analyze', 'extract'],
  '渲染': ['render', 'display', 'view', 'draw'],
  '上传': ['upload', 'import', 'ingest'],
  '下载': ['download', 'export', 'fetch'],
  '删除': ['delete', 'remove', 'destroy', 'drop'],
  '更新': ['update', 'modify', 'patch', 'edit'],
  '创建': ['create', 'add', 'new', 'insert'],
  '查询': ['query', 'search', 'find', 'get', 'fetch'],
  '验证': ['validate', 'verify', 'check', 'assert'],
  '转换': ['convert', 'transform', 'map', 'serialize'],
  '加密': ['encrypt', 'hash', 'cipher', 'crypto'],
  '连接': ['connect', 'connection', 'socket', 'link'],
  '断开': ['disconnect', 'close', 'terminate'],
  '重试': ['retry', 'backoff', 'reconnect'],
  '队列': ['queue', 'buffer', 'fifo'],
  '任务': ['task', 'job', 'mission', 'work'],
  '调度': ['dispatch', 'schedule', 'orchestrate'],
  '编排': ['orchestrate', 'orchestration', 'pipeline'],
  '知识库': ['knowledge', 'knowledgeBase', 'kb'],
  '文件': ['file', 'document', 'asset'],
  '目录': ['directory', 'folder', 'dir'],
  '依赖': ['dependency', 'dep', 'import', 'require'],
  '符号': ['symbol', 'token', 'identifier'],
  '排序': ['sort', 'rank', 'order'],
  '过滤': ['filter', 'exclude', 'whitelist'],
  '分页': ['pagination', 'page', 'paginate', 'offset'],
  '会话': ['session', 'conversation', 'chat'],
  '消息': ['message', 'msg', 'notification'],
  '提示词': ['prompt', 'instruction', 'systemPrompt'],

  // 英文同义词扩展（优化 #3：大幅扩充 + 双向化）
  'auth': ['authentication', 'authorize', 'login', 'signin'],
  'config': ['configuration', 'settings', 'options', 'preferences'],
  'init': ['initialize', 'setup', 'bootstrap', 'startup'],
  'exec': ['execute', 'run', 'invoke', 'perform'],
  'err': ['error', 'exception', 'failure', 'fault'],
  'msg': ['message', 'notification', 'alert'],
  'req': ['request', 'http', 'call'],
  'res': ['response', 'result', 'reply'],
  'ctx': ['context', 'state', 'scope'],
  'cb': ['callback', 'handler', 'listener'],
  'fn': ['function', 'method', 'handler', 'procedure'],
  'args': ['arguments', 'params', 'parameters', 'inputs'],
  'opts': ['options', 'config', 'settings', 'preferences'],
  // 新增：动词族扩展
  'create': ['generate', 'produce', 'build', 'make', 'construct', 'add', 'insert'],
  'remove': ['delete', 'destroy', 'drop', 'unlink', 'erase', 'purge'],
  'update': ['modify', 'patch', 'edit', 'change', 'alter', 'mutate'],
  'get': ['fetch', 'retrieve', 'obtain', 'read', 'load', 'acquire'],
  'set': ['assign', 'store', 'write', 'save', 'put', 'update'],
  'send': ['emit', 'dispatch', 'publish', 'broadcast', 'transmit'],
  'receive': ['accept', 'consume', 'handle', 'process', 'listen'],
  'start': ['begin', 'launch', 'open', 'activate', 'enable'],
  'stop': ['end', 'halt', 'close', 'deactivate', 'disable', 'shutdown'],
  'parse': ['analyze', 'extract', 'decode', 'deserialize', 'interpret'],
  'format': ['serialize', 'encode', 'stringify', 'render', 'template'],
  'validate': ['verify', 'check', 'assert', 'ensure', 'confirm', 'sanitize'],
  'convert': ['transform', 'map', 'translate', 'adapt', 'cast'],
  'find': ['search', 'query', 'lookup', 'locate', 'discover', 'match'],
  'filter': ['exclude', 'select', 'where', 'predicate', 'sieve'],
  'sort': ['order', 'rank', 'arrange', 'compare', 'prioritize'],
  'merge': ['combine', 'join', 'concat', 'aggregate', 'union'],
  'split': ['divide', 'separate', 'partition', 'chunk', 'tokenize'],
  // 新增：名词族扩展
  'cache': ['memo', 'memoize', 'buffer', 'store', 'pool'],
  'queue': ['buffer', 'fifo', 'stack', 'pipe', 'channel'],
  'event': ['signal', 'trigger', 'hook', 'notification', 'action'],
  'error': ['exception', 'failure', 'fault', 'issue', 'bug', 'defect'],
  'log': ['logger', 'logging', 'trace', 'debug', 'audit', 'record'],
  'test': ['spec', 'unittest', 'assert', 'expect', 'mock', 'stub'],
  'route': ['router', 'endpoint', 'path', 'url', 'mapping'],
  'middleware': ['interceptor', 'filter', 'guard', 'plugin', 'hook'],
  'component': ['widget', 'element', 'module', 'block', 'part'],
  'state': ['status', 'store', 'redux', 'atom', 'signal', 'reactive'],
  'database': ['db', 'storage', 'repository', 'datastore', 'persistence'],
  'schema': ['model', 'entity', 'shape', 'definition', 'structure'],
  'token': ['identifier', 'symbol', 'key', 'credential', 'jwt'],
  'stream': ['pipe', 'flow', 'observable', 'channel', 'reader', 'writer'],
  'worker': ['thread', 'process', 'executor', 'runner', 'agent'],
  'promise': ['async', 'await', 'future', 'deferred', 'observable'],
};

// ============================================================================
// QueryExpander 类
// ============================================================================

export class QueryExpander {
  private llmClient: LLMClient | null = null;
  private enableLLM: boolean;
  /** 优化 #3: 反向同义词表（自动从 SYNONYM_MAP 构建） */
  private reverseSynonymMap = new Map<string, string[]>();
  /** 优化 #4: LLM 查询扩展结果缓存 */
  private llmCache = new Map<string, { tokens: string[]; timestamp: number }>();
  private static readonly LLM_CACHE_MAX = 50;
  private static readonly LLM_CACHE_TTL = 300_000; // 5 分钟
  /** 优化 #15: 项目词汇表（由外部注入） */
  private projectVocabulary: Set<string> | null = null;

  constructor(options?: { enableLLM?: boolean }) {
    this.enableLLM = options?.enableLLM ?? true;
    this.buildReverseSynonymMap();
  }

  /**
   * 设置 LLM 客户端
   */
  setLLMClient(client: LLMClient | null): void {
    this.llmClient = client;
  }

  /**
   * 优化 #15: 注入项目高频词汇表
   */
  setProjectVocabulary(vocabulary: Set<string>): void {
    this.projectVocabulary = vocabulary;
  }

  /**
   * 扩展查询
   * @param query 用户原始查询
   * @param originalTokens 分词器已提取的 token
   * @returns 扩展后的查询信息
   */
  async expand(query: string, originalTokens: string[]): Promise<ExpandedQuery> {
    const allTokens = new Set<string>(originalTokens);

    // 1. 离线同义词扩展（始终执行）
    this.offlineExpand(query, originalTokens, allTokens);

    // 2. LLM 在线扩展（可选）
    let mode: ExpandedQuery['mode'] = 'offline';
    if (this.enableLLM && this.llmClient) {
      try {
        const llmTokens = await this.llmExpand(query);
        if (llmTokens.length > 0) {
          for (const t of llmTokens) allTokens.add(t);
          mode = 'hybrid';
        }
      } catch (error) {
        logger.warn('查询扩展.LLM扩展失败，使用离线结果', { error }, LogCategory.SESSION);
      }
    }

    // 控制总量（最多 30 个 token）
    const expandedTokens = Array.from(allTokens).slice(0, 30);

    return {
      original: query,
      expandedTokens,
      mode,
    };
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  /**
   * 离线同义词扩展（优化 #3: 双向查找）
   */
  private offlineExpand(query: string, tokens: string[], result: Set<string>): void {
    const queryLower = query.toLowerCase();

    // 1. 从同义词表匹配中文关键词
    for (const [zhKey, enValues] of Object.entries(SYNONYM_MAP)) {
      if (queryLower.includes(zhKey)) {
        for (const v of enValues) result.add(v.toLowerCase());
      }
    }

    // 2. 对每个 token 查找正向 + 反向同义词
    for (const token of tokens) {
      const tokenLower = token.toLowerCase();
      // 正向查找
      const forwardSynonyms = SYNONYM_MAP[tokenLower];
      if (forwardSynonyms) {
        for (const s of forwardSynonyms) result.add(s.toLowerCase());
      }
      // 反向查找（优化 #3: 如 `authentication` → `auth`）
      const reverseSynonyms = this.reverseSynonymMap.get(tokenLower);
      if (reverseSynonyms) {
        for (const s of reverseSynonyms) result.add(s.toLowerCase());
      }
    }

    // 3. camelCase/PascalCase 拆分
    for (const token of tokens) {
      const parts = this.splitCamelCase(token);
      if (parts.length > 1) {
        for (const p of parts) {
          if (p.length >= 3) result.add(p.toLowerCase());
        }
      }
    }

    // 4. 优化 #15: 项目词汇动态扩展
    if (this.projectVocabulary && this.projectVocabulary.size > 0) {
      for (const token of tokens) {
        const tokenLower = token.toLowerCase();
        if (tokenLower.length < 3) continue;
        // 在项目词汇中查找包含该 token 的标识符
        for (const word of this.projectVocabulary) {
          if (word !== tokenLower && word.includes(tokenLower) && word.length <= tokenLower.length + 15) {
            result.add(word);
          }
        }
      }
    }
  }

  /**
   * LLM 在线扩展（优化 #4: 带缓存）
   */
  private async llmExpand(query: string): Promise<string[]> {
    if (!this.llmClient) return [];

    // 优化 #4: 检查 LLM 缓存
    const cacheKey = query.trim().toLowerCase();
    const cached = this.llmCache.get(cacheKey);
    if (cached && (Date.now() - cached.timestamp) < QueryExpander.LLM_CACHE_TTL) {
      return cached.tokens;
    }

    const prompt = `你是一个代码搜索助手。用户想搜索代码库中的相关代码。
请根据用户的查询意图，生成 5-10 个最可能出现在代码中的英文标识符（函数名、类名、变量名等）。

用户查询: "${query}"

只输出标识符列表，每行一个，不要编号，不要解释：`;

    const response = await this.llmClient.sendMessage({
      messages: [{ role: 'user', content: prompt }],
      maxTokens: 200,
      temperature: 0.3,
    });

    if (!response?.content) return [];

    // 解析 LLM 返回的标识符列表
    const tokens = response.content
      .split('\n')
      .map((line: string) => line.trim())
      .filter((line: string) => line.length >= 2 && line.length <= 60 && /^[a-zA-Z_$][a-zA-Z0-9_$]*$/.test(line))
      .map((line: string) => line.toLowerCase());

    // 优化 #4: 写入 LLM 缓存
    this.llmCache.set(cacheKey, { tokens, timestamp: Date.now() });
    // LRU 淘汰
    if (this.llmCache.size > QueryExpander.LLM_CACHE_MAX) {
      const firstKey = this.llmCache.keys().next().value;
      if (firstKey !== undefined) this.llmCache.delete(firstKey);
    }

    return tokens;
  }

  /**
   * 优化 #3: 构建反向同义词表
   * SYNONYM_MAP: key → [val1, val2] → 生成 val1 → [key], val2 → [key]
   */
  private buildReverseSynonymMap(): void {
    for (const [key, values] of Object.entries(SYNONYM_MAP)) {
      for (const val of values) {
        const valLower = val.toLowerCase();
        const existing = this.reverseSynonymMap.get(valLower) || [];
        if (!existing.includes(key.toLowerCase())) {
          existing.push(key.toLowerCase());
        }
        this.reverseSynonymMap.set(valLower, existing);
      }
    }
  }

  /**
   * 拆分 camelCase / PascalCase
   */
  private splitCamelCase(str: string): string[] {
    return str
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .replace(/([A-Z])([A-Z][a-z])/g, '$1 $2')
      .split(/[\s_-]+/)
      .filter(s => s.length > 0);
  }
}