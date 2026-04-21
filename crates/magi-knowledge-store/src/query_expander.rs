use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WeightHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_match: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tfidf: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centrality: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recency: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_weight: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ExpandedQuery {
    pub original: String,
    pub expanded_tokens: Vec<String>,
    pub mode: ExpandMode,
    pub weight_hints: Option<WeightHints>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpandMode {
    Offline,
    Hybrid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmExpandResult {
    pub tokens: Vec<String>,
    pub weight_hints: Option<WeightHints>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExpansionCacheSnapshot {
    pub entries: Vec<(String, LlmExpandCacheEntry)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmExpandCacheEntry {
    pub tokens: Vec<String>,
    pub weight_hints: Option<WeightHints>,
    pub timestamp: u64,
}

const LLM_CACHE_MAX: usize = 50;
const LLM_CACHE_TTL_MS: u64 = 300_000;
const MAX_EXPANDED_TOKENS: usize = 30;

pub struct QueryExpander {
    reverse_synonym_map: HashMap<String, Vec<String>>,
    project_vocabulary: Option<HashSet<String>>,
    llm_cache: HashMap<String, LlmExpandCacheEntry>,
    camel_case_re: Regex,
    camel_case_re2: Regex,
}

impl Default for QueryExpander {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryExpander {
    pub fn new() -> Self {
        let reverse = build_reverse_synonym_map();
        Self {
            reverse_synonym_map: reverse,
            project_vocabulary: None,
            llm_cache: HashMap::new(),
            camel_case_re: Regex::new(r"([a-z])([A-Z])").unwrap(),
            camel_case_re2: Regex::new(r"([A-Z])([A-Z][a-z])").unwrap(),
        }
    }

    pub fn set_project_vocabulary(&mut self, vocab: HashSet<String>) {
        self.project_vocabulary = Some(vocab);
    }

    pub fn expand_offline(&self, query: &str, original_tokens: &[String]) -> ExpandedQuery {
        let mut all_tokens: HashSet<String> = original_tokens.iter().cloned().collect();

        self.offline_expand(query, original_tokens, &mut all_tokens);

        let expanded_tokens: Vec<String> = all_tokens.into_iter().take(MAX_EXPANDED_TOKENS).collect();

        ExpandedQuery {
            original: query.to_string(),
            expanded_tokens,
            mode: ExpandMode::Offline,
            weight_hints: None,
        }
    }

    pub fn merge_llm_result(&self, expanded: &mut ExpandedQuery, result: LlmExpandResult) {
        if !result.tokens.is_empty() {
            let mut token_set: HashSet<String> = expanded.expanded_tokens.iter().cloned().collect();
            for t in &result.tokens {
                token_set.insert(t.clone());
            }
            expanded.expanded_tokens = token_set.into_iter().take(MAX_EXPANDED_TOKENS).collect();
            expanded.mode = ExpandMode::Hybrid;
        }
        if result.weight_hints.is_some() {
            expanded.weight_hints = result.weight_hints;
        }
    }

    pub fn build_llm_prompt(&self, query: &str) -> String {
        format!(
            r#"你是代码搜索引擎的查询分析器。分析用户查询意图，输出结构化 JSON。

用户查询: "{}"

输出严格 JSON（无多余文字）:
{{
  "identifiers": ["5-10个最可能出现在代码中的英文标识符"],
  "filePatterns": ["0-3个可能的文件名片段，如 auth, middleware"],
  "focus": "symbol|semantic"
}}

规则:
- identifiers: 函数名/类名/变量名/方法名，英文
- filePatterns: 文件路径中可能包含的关键词
- focus: symbol=查找特定符号定义, semantic=理解功能逻辑"#,
            query
        )
    }

    pub fn parse_llm_response(&self, content: &str) -> LlmExpandResult {
        if let Some(result) = self.parse_structured_response(content) {
            return result;
        }
        self.fallback_parse_tokens(content)
    }

    pub fn get_cached_llm_result(&self, query: &str) -> Option<&LlmExpandCacheEntry> {
        let key = query.trim().to_lowercase();
        let entry = self.llm_cache.get(&key)?;
        let now = now_millis();
        if now - entry.timestamp < LLM_CACHE_TTL_MS {
            Some(entry)
        } else {
            None
        }
    }

    pub fn cache_llm_result(&mut self, query: &str, result: &LlmExpandResult) {
        let key = query.trim().to_lowercase();
        self.llm_cache.insert(
            key,
            LlmExpandCacheEntry {
                tokens: result.tokens.clone(),
                weight_hints: result.weight_hints.clone(),
                timestamp: now_millis(),
            },
        );
        if self.llm_cache.len() > LLM_CACHE_MAX {
            if let Some(oldest_key) = self.llm_cache.keys().next().cloned() {
                self.llm_cache.remove(&oldest_key);
            }
        }
    }

    pub fn export_cache(&self) -> ExpansionCacheSnapshot {
        ExpansionCacheSnapshot {
            entries: self
                .llm_cache
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    pub fn import_cache(&mut self, snapshot: ExpansionCacheSnapshot) {
        let now = now_millis();
        for (key, entry) in snapshot.entries {
            if now - entry.timestamp < LLM_CACHE_TTL_MS {
                self.llm_cache.insert(key, entry);
            }
        }
    }

    fn offline_expand(
        &self,
        query: &str,
        tokens: &[String],
        result: &mut HashSet<String>,
    ) {
        let query_lower = query.to_lowercase();

        for (zh_key, en_values) in SYNONYM_ENTRIES {
            if query_lower.contains(zh_key) {
                for v in *en_values {
                    result.insert(v.to_lowercase());
                }
            }
        }

        for token in tokens {
            let token_lower = token.to_lowercase();
            for (key, values) in SYNONYM_ENTRIES {
                if *key == token_lower.as_str() {
                    for v in *values {
                        result.insert(v.to_lowercase());
                    }
                }
            }
            if let Some(reverse_synonyms) = self.reverse_synonym_map.get(&token_lower) {
                for s in reverse_synonyms {
                    result.insert(s.clone());
                }
            }
        }

        for token in tokens {
            let parts = self.split_camel_case(token);
            if parts.len() > 1 {
                for p in &parts {
                    if p.len() >= 3 {
                        result.insert(p.to_lowercase());
                    }
                }
            }
        }

        if let Some(ref vocab) = self.project_vocabulary {
            if !vocab.is_empty() {
                for token in tokens {
                    let token_lower = token.to_lowercase();
                    if token_lower.len() < 3 {
                        continue;
                    }
                    for word in vocab {
                        if word != &token_lower
                            && word.contains(&token_lower)
                            && word.len() <= token_lower.len() + 15
                        {
                            result.insert(word.clone());
                        }
                    }
                }
            }
        }
    }

    fn parse_structured_response(&self, content: &str) -> Option<LlmExpandResult> {
        let json_re = Regex::new(r"\{[\s\S]*\}").ok()?;
        let json_match = json_re.find(content)?;
        let parsed: serde_json::Value = serde_json::from_str(json_match.as_str()).ok()?;

        let mut tokens = Vec::new();

        if let Some(identifiers) = parsed.get("identifiers").and_then(|v| v.as_array()) {
            for id in identifiers {
                if let Some(s) = id.as_str() {
                    if s.len() >= 2 && s.len() <= 60 {
                        tokens.push(s.to_lowercase());
                    }
                }
            }
        }

        if let Some(patterns) = parsed.get("filePatterns").and_then(|v| v.as_array()) {
            for fp in patterns {
                if let Some(s) = fp.as_str() {
                    if s.len() >= 2 && s.len() <= 40 {
                        tokens.push(s.to_lowercase());
                    }
                }
            }
        }

        let weight_hints = match parsed.get("focus").and_then(|v| v.as_str()) {
            Some("symbol") => Some(WeightHints {
                symbol_match: Some(0.45),
                tfidf: Some(0.20),
                position_weight: Some(0.15),
                centrality: Some(0.08),
                recency: Some(0.05),
                type_weight: Some(0.07),
            }),
            _ => None,
        };

        Some(LlmExpandResult {
            tokens,
            weight_hints,
        })
    }

    fn fallback_parse_tokens(&self, content: &str) -> LlmExpandResult {
        let ident_re = Regex::new(r"^[a-zA-Z_$][a-zA-Z0-9_$]*$").unwrap();
        let tokens: Vec<String> = content
            .lines()
            .map(|l| l.trim())
            .filter(|l| l.len() >= 2 && l.len() <= 60 && ident_re.is_match(l))
            .map(|l| l.to_lowercase())
            .collect();
        LlmExpandResult {
            tokens,
            weight_hints: None,
        }
    }

    fn split_camel_case(&self, s: &str) -> Vec<String> {
        let step1 = self.camel_case_re.replace_all(s, "$1 $2");
        let step2 = self.camel_case_re2.replace_all(&step1, "$1 $2");
        step2
            .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn build_reverse_synonym_map() -> HashMap<String, Vec<String>> {
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for (key, values) in SYNONYM_ENTRIES {
        for val in *values {
            let val_lower = val.to_lowercase();
            let key_lower = key.to_lowercase();
            let entry = reverse.entry(val_lower).or_default();
            if !entry.contains(&key_lower) {
                entry.push(key_lower);
            }
        }
    }
    reverse
}

static SYNONYM_ENTRIES: &[(&str, &[&str])] = &[
    ("登录", &["login", "auth", "signin", "authenticate"]),
    ("注册", &["register", "signup", "createaccount"]),
    ("用户", &["user", "account", "profile"]),
    ("密码", &["password", "credential", "secret"]),
    ("权限", &["permission", "authorization", "access", "role"]),
    ("配置", &["config", "configuration", "settings", "options"]),
    ("数据库", &["database", "db", "storage", "repository"]),
    ("缓存", &["cache", "memo", "memoize"]),
    ("错误", &["error", "exception", "fault", "failure"]),
    ("日志", &["log", "logger", "logging", "trace"]),
    ("测试", &["test", "spec", "unittest", "jest"]),
    ("路由", &["route", "router", "routing", "path"]),
    ("请求", &["request", "req", "fetch", "http"]),
    ("响应", &["response", "res", "reply"]),
    ("模型", &["model", "schema", "entity"]),
    ("服务", &["service", "provider", "handler"]),
    ("控制器", &["controller", "handler", "endpoint"]),
    ("中间件", &["middleware", "interceptor", "filter"]),
    ("组件", &["component", "widget", "element"]),
    ("状态", &["state", "status", "store"]),
    ("事件", &["event", "emitter", "listener", "handler"]),
    ("接口", &["interface", "api", "contract", "protocol"]),
    ("类型", &["type", "typedef", "typing"]),
    ("工具", &["tool", "util", "utility", "helper"]),
    ("搜索", &["search", "find", "query", "lookup"]),
    ("索引", &["index", "indexing", "inverted"]),
    ("解析", &["parse", "parser", "analyze", "extract"]),
    ("渲染", &["render", "display", "view", "draw"]),
    ("上传", &["upload", "import", "ingest"]),
    ("下载", &["download", "export", "fetch"]),
    ("删除", &["delete", "remove", "destroy", "drop"]),
    ("更新", &["update", "modify", "patch", "edit"]),
    ("创建", &["create", "add", "new", "insert"]),
    ("查询", &["query", "search", "find", "get", "fetch"]),
    ("验证", &["validate", "verify", "check", "assert"]),
    ("转换", &["convert", "transform", "map", "serialize"]),
    ("加密", &["encrypt", "hash", "cipher", "crypto"]),
    ("连接", &["connect", "connection", "socket", "link"]),
    ("断开", &["disconnect", "close", "terminate"]),
    ("重试", &["retry", "backoff", "reconnect"]),
    ("队列", &["queue", "buffer", "fifo"]),
    ("任务", &["task", "job", "mission", "work"]),
    ("调度", &["dispatch", "schedule", "orchestrate"]),
    ("编排", &["orchestrate", "orchestration", "pipeline"]),
    ("知识库", &["knowledge", "knowledgebase", "kb"]),
    ("文件", &["file", "document", "asset"]),
    ("目录", &["directory", "folder", "dir"]),
    ("依赖", &["dependency", "dep", "import", "require"]),
    ("符号", &["symbol", "token", "identifier"]),
    ("排序", &["sort", "rank", "order"]),
    ("过滤", &["filter", "exclude", "whitelist"]),
    ("分页", &["pagination", "page", "paginate", "offset"]),
    ("会话", &["session", "conversation", "chat"]),
    ("消息", &["message", "msg", "notification"]),
    ("提示词", &["prompt", "instruction", "systemprompt"]),
    // English synonym expansions
    ("auth", &["authentication", "authorize", "login", "signin"]),
    ("config", &["configuration", "settings", "options", "preferences"]),
    ("init", &["initialize", "setup", "bootstrap", "startup"]),
    ("exec", &["execute", "run", "invoke", "perform"]),
    ("err", &["error", "exception", "failure", "fault"]),
    ("msg", &["message", "notification", "alert"]),
    ("req", &["request", "http", "call"]),
    ("res", &["response", "result", "reply"]),
    ("ctx", &["context", "state", "scope"]),
    ("cb", &["callback", "handler", "listener"]),
    ("fn", &["function", "method", "handler", "procedure"]),
    ("args", &["arguments", "params", "parameters", "inputs"]),
    ("opts", &["options", "config", "settings", "preferences"]),
    ("create", &["generate", "produce", "build", "make", "construct", "add", "insert"]),
    ("remove", &["delete", "destroy", "drop", "unlink", "erase", "purge"]),
    ("update", &["modify", "patch", "edit", "change", "alter", "mutate"]),
    ("get", &["fetch", "retrieve", "obtain", "read", "load", "acquire"]),
    ("set", &["assign", "store", "write", "save", "put", "update"]),
    ("send", &["emit", "dispatch", "publish", "broadcast", "transmit"]),
    ("receive", &["accept", "consume", "handle", "process", "listen"]),
    ("start", &["begin", "launch", "open", "activate", "enable"]),
    ("stop", &["end", "halt", "close", "deactivate", "disable", "shutdown"]),
    ("parse", &["analyze", "extract", "decode", "deserialize", "interpret"]),
    ("format", &["serialize", "encode", "stringify", "render", "template"]),
    ("validate", &["verify", "check", "assert", "ensure", "confirm", "sanitize"]),
    ("convert", &["transform", "map", "translate", "adapt", "cast"]),
    ("find", &["search", "query", "lookup", "locate", "discover", "match"]),
    ("filter", &["exclude", "select", "where", "predicate", "sieve"]),
    ("sort", &["order", "rank", "arrange", "compare", "prioritize"]),
    ("merge", &["combine", "join", "concat", "aggregate", "union"]),
    ("split", &["divide", "separate", "partition", "chunk", "tokenize"]),
    ("cache", &["memo", "memoize", "buffer", "store", "pool"]),
    ("queue", &["buffer", "fifo", "stack", "pipe", "channel"]),
    ("event", &["signal", "trigger", "hook", "notification", "action"]),
    ("error", &["exception", "failure", "fault", "issue", "bug", "defect"]),
    ("log", &["logger", "logging", "trace", "debug", "audit", "record"]),
    ("test", &["spec", "unittest", "assert", "expect", "mock", "stub"]),
    ("route", &["router", "endpoint", "path", "url", "mapping"]),
    ("middleware", &["interceptor", "filter", "guard", "plugin", "hook"]),
    ("component", &["widget", "element", "module", "block", "part"]),
    ("state", &["status", "store", "redux", "atom", "signal", "reactive"]),
    ("database", &["db", "storage", "repository", "datastore", "persistence"]),
    ("schema", &["model", "entity", "shape", "definition", "structure"]),
    ("token", &["identifier", "symbol", "key", "credential", "jwt"]),
    ("stream", &["pipe", "flow", "observable", "channel", "reader", "writer"]),
    ("worker", &["thread", "process", "executor", "runner", "agent"]),
    ("promise", &["async", "await", "future", "deferred", "observable"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_offline_expand_chinese() {
        let expander = QueryExpander::new();
        let result = expander.expand_offline("查找登录逻辑", &[]);
        assert!(result.expanded_tokens.iter().any(|t| t == "login" || t == "auth"));
    }

    #[test]
    fn test_offline_expand_english_synonym() {
        let expander = QueryExpander::new();
        let result = expander.expand_offline("auth module", &["auth".into()]);
        assert!(result.expanded_tokens.contains(&"authentication".to_string()));
    }

    #[test]
    fn test_reverse_synonym() {
        let expander = QueryExpander::new();
        let result = expander.expand_offline("", &["authentication".into()]);
        assert!(result.expanded_tokens.contains(&"auth".to_string()));
    }

    #[test]
    fn test_camel_case_split() {
        let expander = QueryExpander::new();
        let result = expander.expand_offline("", &["getUserProfile".into()]);
        assert!(result.expanded_tokens.contains(&"user".to_string()));
        assert!(result.expanded_tokens.contains(&"profile".to_string()));
    }

    #[test]
    fn test_project_vocabulary() {
        let mut expander = QueryExpander::new();
        let mut vocab = HashSet::new();
        vocab.insert("authmanager".to_string());
        vocab.insert("authhandler".to_string());
        expander.set_project_vocabulary(vocab);
        let result = expander.expand_offline("", &["auth".into()]);
        assert!(result.expanded_tokens.contains(&"authmanager".to_string()));
    }

    #[test]
    fn test_parse_llm_structured_response() {
        let expander = QueryExpander::new();
        let response = r#"{"identifiers": ["handleAuth", "loginUser"], "filePatterns": ["auth"], "focus": "symbol"}"#;
        let result = expander.parse_llm_response(response);
        assert!(result.tokens.contains(&"handleauth".to_string()));
        assert!(result.weight_hints.is_some());
        assert_eq!(result.weight_hints.unwrap().symbol_match, Some(0.45));
    }

    #[test]
    fn test_parse_llm_fallback() {
        let expander = QueryExpander::new();
        let response = "handleAuth\nloginUser\n123\n";
        let result = expander.parse_llm_response(response);
        assert!(result.tokens.contains(&"handleauth".to_string()));
        assert!(result.tokens.contains(&"loginuser".to_string()));
    }

    #[test]
    fn test_llm_cache() {
        let mut expander = QueryExpander::new();
        let result = LlmExpandResult {
            tokens: vec!["cached_token".into()],
            weight_hints: None,
        };
        expander.cache_llm_result("test query", &result);
        let cached = expander.get_cached_llm_result("test query");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().tokens, vec!["cached_token"]);
    }

    #[test]
    fn test_merge_llm_result() {
        let expander = QueryExpander::new();
        let mut expanded = expander.expand_offline("test", &["test".into()]);
        let llm_result = LlmExpandResult {
            tokens: vec!["extra_token".into()],
            weight_hints: Some(WeightHints {
                symbol_match: Some(0.5),
                ..Default::default()
            }),
        };
        expander.merge_llm_result(&mut expanded, llm_result);
        assert_eq!(expanded.mode, ExpandMode::Hybrid);
        assert!(expanded.expanded_tokens.contains(&"extra_token".to_string()));
        assert!(expanded.weight_hints.is_some());
    }

    #[test]
    fn test_export_import_cache() {
        let mut expander = QueryExpander::new();
        expander.cache_llm_result("q1", &LlmExpandResult {
            tokens: vec!["t1".into()],
            weight_hints: None,
        });
        let snapshot = expander.export_cache();
        assert_eq!(snapshot.entries.len(), 1);

        let mut expander2 = QueryExpander::new();
        expander2.import_cache(snapshot);
        assert!(expander2.get_cached_llm_result("q1").is_some());
    }
}
