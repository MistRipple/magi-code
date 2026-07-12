use std::collections::{HashMap, HashSet};

use regex::Regex;

#[derive(Clone, Debug)]
pub struct ExpandedQuery {
    pub expanded_tokens: Vec<String>,
}

const MAX_EXPANDED_TOKENS: usize = 30;

pub struct QueryExpander {
    reverse_synonym_map: HashMap<String, Vec<String>>,
    project_vocabulary: Option<HashSet<String>>,
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

        let mut expanded_tokens = Vec::with_capacity(MAX_EXPANDED_TOKENS);
        for token in original_tokens {
            if !expanded_tokens.contains(token) {
                expanded_tokens.push(token.clone());
            }
        }
        let mut additions = all_tokens
            .into_iter()
            .filter(|token| !expanded_tokens.contains(token))
            .collect::<Vec<_>>();
        additions.sort();
        expanded_tokens.extend(additions);
        expanded_tokens.truncate(MAX_EXPANDED_TOKENS);

        ExpandedQuery { expanded_tokens }
    }

    fn offline_expand(&self, query: &str, tokens: &[String], result: &mut HashSet<String>) {
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

        if let Some(ref vocab) = self.project_vocabulary
            && !vocab.is_empty()
        {
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
    (
        "config",
        &["configuration", "settings", "options", "preferences"],
    ),
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
    (
        "create",
        &[
            "generate",
            "produce",
            "build",
            "make",
            "construct",
            "add",
            "insert",
        ],
    ),
    (
        "remove",
        &["delete", "destroy", "drop", "unlink", "erase", "purge"],
    ),
    (
        "update",
        &["modify", "patch", "edit", "change", "alter", "mutate"],
    ),
    (
        "get",
        &["fetch", "retrieve", "obtain", "read", "load", "acquire"],
    ),
    (
        "set",
        &["assign", "store", "write", "save", "put", "update"],
    ),
    (
        "send",
        &["emit", "dispatch", "publish", "broadcast", "transmit"],
    ),
    (
        "receive",
        &["accept", "consume", "handle", "process", "listen"],
    ),
    ("start", &["begin", "launch", "open", "activate", "enable"]),
    (
        "stop",
        &["end", "halt", "close", "deactivate", "disable", "shutdown"],
    ),
    (
        "parse",
        &["analyze", "extract", "decode", "deserialize", "interpret"],
    ),
    (
        "format",
        &["serialize", "encode", "stringify", "render", "template"],
    ),
    (
        "validate",
        &["verify", "check", "assert", "ensure", "confirm", "sanitize"],
    ),
    (
        "convert",
        &["transform", "map", "translate", "adapt", "cast"],
    ),
    (
        "find",
        &["search", "query", "lookup", "locate", "discover", "match"],
    ),
    (
        "filter",
        &["exclude", "select", "where", "predicate", "sieve"],
    ),
    (
        "sort",
        &["order", "rank", "arrange", "compare", "prioritize"],
    ),
    (
        "merge",
        &["combine", "join", "concat", "aggregate", "union"],
    ),
    (
        "split",
        &["divide", "separate", "partition", "chunk", "tokenize"],
    ),
    ("cache", &["memo", "memoize", "buffer", "store", "pool"]),
    ("queue", &["buffer", "fifo", "stack", "pipe", "channel"]),
    (
        "event",
        &["signal", "trigger", "hook", "notification", "action"],
    ),
    (
        "error",
        &["exception", "failure", "fault", "issue", "bug", "defect"],
    ),
    (
        "log",
        &["logger", "logging", "trace", "debug", "audit", "record"],
    ),
    (
        "test",
        &["spec", "unittest", "assert", "expect", "mock", "stub"],
    ),
    ("route", &["router", "endpoint", "path", "url", "mapping"]),
    (
        "middleware",
        &["interceptor", "filter", "guard", "plugin", "hook"],
    ),
    (
        "component",
        &["widget", "element", "module", "block", "part"],
    ),
    (
        "state",
        &["status", "store", "redux", "atom", "signal", "reactive"],
    ),
    (
        "database",
        &["db", "storage", "repository", "datastore", "persistence"],
    ),
    (
        "schema",
        &["model", "entity", "shape", "definition", "structure"],
    ),
    (
        "token",
        &["identifier", "symbol", "key", "credential", "jwt"],
    ),
    (
        "stream",
        &["pipe", "flow", "observable", "channel", "reader", "writer"],
    ),
    (
        "worker",
        &["thread", "process", "executor", "runner", "agent"],
    ),
    (
        "promise",
        &["async", "await", "future", "deferred", "observable"],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_offline_expand_chinese() {
        let expander = QueryExpander::new();
        let result = expander.expand_offline("查找登录逻辑", &[]);
        assert!(
            result
                .expanded_tokens
                .iter()
                .any(|t| t == "login" || t == "auth")
        );
    }

    #[test]
    fn test_offline_expand_english_synonym() {
        let expander = QueryExpander::new();
        let result = expander.expand_offline("auth module", &["auth".into()]);
        assert!(
            result
                .expanded_tokens
                .contains(&"authentication".to_string())
        );
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
    fn offline_expansion_is_deterministic_and_keeps_original_terms_first() {
        let expander = QueryExpander::new();
        let original = vec!["session".to_string(), "model".to_string()];
        let first = expander.expand_offline("session model", &original);

        for _ in 0..20 {
            let next = expander.expand_offline("session model", &original);
            assert_eq!(next.expanded_tokens, first.expanded_tokens);
        }
        assert_eq!(&first.expanded_tokens[..2], original.as_slice());
    }
}
