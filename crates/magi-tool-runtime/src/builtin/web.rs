//! `web_search` / `web_fetch`：联网工具的唯一实现。
//!
//! 分三层，每一层只做一件事：
//! 1. HTTP 访问层（[`get_with_retry`]）：共享客户端、总预算内的暂态重试、失败分类。
//!    暂态网络故障的重试只发生在这里；上层（模型 / 运行时）不应再靠重复调用同一参数来「碰运气」，
//!    运行时对相同参数连续失败的止损也依赖这一点——失败信息必须明确告诉模型「已经重试过」。
//! 2. 搜索来源（[`SearchSource`]）：给定关键词，只负责拼出请求地址、把返回页面解析成结果。
//!    新增或更换来源不触碰重试、预算、相关性检查和错误结构。
//! 3. 工具入口（[`execute_web_search`] / [`execute_web_fetch`]）：参数、来源调度、结果校验、输出合同。
//!
//! 返回给模型的失败信息只描述失败类别（超时、域名无法解析、HTTP 状态码……），
//! 不暴露底层错误文本、内部地址或原始响应；完整的错误链只写日志。

use super::{failure::ToolFailure, parse_json_object, required_string_field};
use crate::BuiltinToolAccessMode;
use base64::Engine as _;
use reqwest::{
    Url,
    blocking::{Client, Response},
    header::RETRY_AFTER,
};
use scraper::{ElementRef, Html, Selector};
use serde_json::json;
use std::{
    error::Error as StdError,
    io::Read,
    sync::{LazyLock, OnceLock},
    time::{Duration, Instant},
};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// 单次搜索（含所有来源、所有重试）的总预算。
const SEARCH_BUDGET: Duration = Duration::from_secs(24);
const SEARCH_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次抓取（含重试）的总预算。
const FETCH_BUDGET: Duration = Duration::from_secs(45);
const FETCH_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);
/// 同一地址最多请求的次数（含首次）。
const MAX_ATTEMPTS: u32 = 3;
const RETRY_BACKOFF: [Duration; 2] = [Duration::from_millis(400), Duration::from_millis(1_200)];
/// 剩余预算不足以完成一次有意义的请求时不再发起重试。
const MIN_ATTEMPT_WINDOW: Duration = Duration::from_secs(3);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3);
const MAX_SEARCH_RESULTS: usize = 10;
const FETCH_MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const FETCH_MAX_CONTENT_CHARS: usize = 50_000;

// ══════════════════════════════════════════════════════════════════════════════
// 失败分类
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WebFailure {
    Timeout,
    Dns,
    Connect,
    Tls,
    /// 连接中途断开等无法进一步归类的网络错误。
    Network,
    Status(u16),
    /// 对方要求人机验证。
    Blocked,
    /// 页面可以取到，但不是预期的结构。
    Unrecognized,
    /// 拿到了结果，但与关键词明显不相关。
    Irrelevant,
    UnsupportedContent(String),
    InvalidUrl,
    TooManyRedirects,
    Internal,
}

impl WebFailure {
    /// 值得由访问层自动重试的暂态故障。
    fn transient(&self) -> bool {
        match self {
            Self::Timeout | Self::Dns | Self::Connect | Self::Network => true,
            Self::Status(code) => matches!(code, 408 | 425 | 429 | 500 | 502 | 503 | 504),
            _ => false,
        }
    }

    fn slug(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Dns => "dns_failed",
            Self::Connect => "connect_failed",
            Self::Tls => "tls_failed",
            Self::Network => "network_error",
            Self::Status(_) => "http_error",
            Self::Blocked => "blocked",
            Self::Unrecognized => "unrecognized_response",
            Self::Irrelevant => "irrelevant_results",
            Self::UnsupportedContent(_) => "unsupported_content",
            Self::InvalidUrl => "invalid_url",
            Self::TooManyRedirects => "too_many_redirects",
            Self::Internal => "internal_error",
        }
    }

    /// 面向模型的失败类别说明：只描述类别，不带底层错误文本。
    fn describe(&self) -> String {
        match self {
            Self::Timeout => "请求超时".to_string(),
            Self::Dns => "域名无法解析".to_string(),
            Self::Connect => "无法建立连接".to_string(),
            Self::Tls => "安全连接（TLS）失败".to_string(),
            Self::Network => "网络连接中断".to_string(),
            Self::Status(code) => format!("返回 HTTP {code}"),
            Self::Blocked => "被人机验证拦截".to_string(),
            Self::Unrecognized => "返回了无法识别的页面".to_string(),
            Self::Irrelevant => "返回的结果与关键词不相关".to_string(),
            Self::UnsupportedContent(content_type) => {
                format!("内容类型 {content_type} 不是网页或文本")
            }
            Self::InvalidUrl => "网址无效（仅支持 http / https）".to_string(),
            Self::TooManyRedirects => "重定向次数过多".to_string(),
            Self::Internal => "内部初始化失败".to_string(),
        }
    }

    fn http_status(&self) -> Option<u16> {
        match self {
            Self::Status(code) => Some(*code),
            _ => None,
        }
    }
}

fn error_chain(error: &dyn StdError) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(" <- ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn classify_request_error(error: &reqwest::Error) -> WebFailure {
    let chain = error_chain(error).to_ascii_lowercase();
    if error.is_timeout() || chain.contains("timed out") || chain.contains("deadline has elapsed") {
        WebFailure::Timeout
    } else if error.is_redirect() {
        WebFailure::TooManyRedirects
    } else if error.is_builder() {
        WebFailure::InvalidUrl
    } else if chain.contains("dns error")
        || chain.contains("failed to lookup address")
        || chain.contains("name or service not known")
        || chain.contains("nodename nor servname")
        || chain.contains("no such host")
    {
        WebFailure::Dns
    } else if chain.contains("certificate") || chain.contains("tls") || chain.contains("ssl") {
        WebFailure::Tls
    } else if error.is_connect() {
        WebFailure::Connect
    } else {
        WebFailure::Network
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// HTTP 访问层
// ══════════════════════════════════════════════════════════════════════════════

fn http_client() -> Result<&'static Client, WebFailure> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|error| error_chain(&error))
    }) {
        Ok(client) => Ok(client),
        Err(error) => {
            tracing::warn!(error = %error, "web http client initialization failed");
            Err(WebFailure::Internal)
        }
    }
}

/// 一次工具调用的时间预算：所有请求、重试、来源切换共用同一个截止时间。
struct Budget {
    deadline: Instant,
    attempt_timeout: Duration,
}

impl Budget {
    fn new(total: Duration, attempt_timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
            attempt_timeout,
        }
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

struct GetFailure {
    failure: WebFailure,
    /// 实际发出的请求次数。
    attempts: u32,
}

fn retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

/// 带重试的 GET：暂态故障在预算内最多请求 [`MAX_ATTEMPTS`] 次；成功状态才返回响应。
fn get_with_retry(tool: &str, url: &str, budget: &Budget) -> Result<(Response, u32), GetFailure> {
    let client = http_client().map_err(|failure| GetFailure {
        failure,
        attempts: 0,
    })?;
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        // 重试前已确认剩余预算足够；首次请求即使预算很少也照常发起，由调用方决定是否还值得尝试。
        let timeout = budget.attempt_timeout.min(budget.remaining());
        let mut wait = RETRY_BACKOFF[(attempts as usize - 1).min(RETRY_BACKOFF.len() - 1)];
        let failure = match client.get(url).timeout(timeout).send() {
            Ok(response) if response.status().is_success() => return Ok((response, attempts)),
            Ok(response) => {
                if let Some(requested) = retry_after(&response) {
                    wait = requested;
                }
                WebFailure::Status(response.status().as_u16())
            }
            Err(error) => {
                let failure = classify_request_error(&error);
                tracing::warn!(
                    tool,
                    attempt = attempts,
                    failure = failure.slug(),
                    error = %error_chain(&error),
                    "web request failed"
                );
                failure
            }
        };
        let can_retry = failure.transient()
            && attempts < MAX_ATTEMPTS
            && budget.remaining() >= wait + MIN_ATTEMPT_WINDOW;
        if !can_retry {
            return Err(GetFailure { failure, attempts });
        }
        std::thread::sleep(wait);
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 失败输出合同
// ══════════════════════════════════════════════════════════════════════════════

fn retried_suffix(attempts: u32) -> String {
    if attempts > 1 {
        format!("（已自动重试 {} 次）", attempts - 1)
    } else {
        String::new()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 文本工具
// ══════════════════════════════════════════════════════════════════════════════

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn element_text(element: ElementRef<'_>) -> String {
    collapse_whitespace(&element.text().collect::<String>())
}

/// 把一段 HTML 片段转成纯文本：标签被剥离，实体（含 `&#0183;` 这类数字实体）只解码一次。
fn html_fragment_text(fragment: &str) -> String {
    Html::parse_fragment(fragment)
        .root_element()
        .text()
        .collect::<String>()
}

static PUBLISHED_PREFIX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?P<date>\d{4}年\d{1,2}月\d{1,2}日|[A-Z][a-z]{2,8}\.? \d{1,2}, \d{4}|\d{4}-\d{2}-\d{2}|\d+\s*(?:秒|分钟|小时|天|周|个月|年)前|\d+\s+(?:second|minute|hour|day|week|month|year)s?\s+ago)\s*[·\-–—]\s*(?P<rest>.+)$",
    )
    .expect("发布时间前缀正则必须合法")
});

/// 搜索摘要常以「日期 · 正文」开头：把日期拆成独立字段，正文里不再混着它。
fn split_published_prefix(text: &str) -> (Option<String>, String) {
    match PUBLISHED_PREFIX.captures(text) {
        Some(captures) => (
            Some(captures["date"].to_string()),
            captures["rest"].trim().to_string(),
        ),
        None => (None, text.to_string()),
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 搜索来源
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    published: Option<String>,
}

pub(super) enum SourceParse {
    Results(Vec<SearchResult>),
    /// 来源明确表示没有结果。
    Empty,
    Blocked,
    Unrecognized,
}

/// 搜索来源：只负责「关键词 → 请求地址」与「页面 → 结果」。
pub(super) trait SearchSource: Sync {
    fn name(&self) -> &'static str;
    fn url(&self, query: &str) -> String;
    fn parse(&self, html: &str) -> SourceParse;
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("内置 CSS 选择器必须合法")
}

fn push_result(results: &mut Vec<SearchResult>, title: String, url: String, text: String) {
    if title.is_empty() || !url.starts_with("http") {
        return;
    }
    if results.iter().any(|existing| existing.url == url) {
        return;
    }
    let (published, snippet) = split_published_prefix(&text);
    results.push(SearchResult {
        title,
        url,
        snippet,
        published,
    });
}

struct Bing;

static BING_ITEM: LazyLock<Selector> = LazyLock::new(|| selector("li.b_algo"));
static BING_LINK: LazyLock<Selector> = LazyLock::new(|| selector("h2 a[href]"));
static BING_SNIPPETS: LazyLock<Vec<Selector>> = LazyLock::new(|| {
    [
        ".b_caption p",
        ".b_lineclamp2",
        ".b_lineclamp3",
        ".b_lineclamp4",
        ".b_algoSlug",
        "p",
    ]
    .into_iter()
    .map(selector)
    .collect()
});

impl SearchSource for Bing {
    fn name(&self) -> &'static str {
        "bing"
    }

    /// 不指定语言和地区：Bing 会按访问者所在区域选择市场。写死 `cc=us&setlang=en-us`
    /// 会让中文关键词返回与关键词无关的页面。
    fn url(&self, query: &str) -> String {
        format!(
            "https://www.bing.com/search?q={}",
            urlencoding::encode(query)
        )
    }

    fn parse(&self, html: &str) -> SourceParse {
        let lower = html.to_ascii_lowercase();
        if lower.contains("b_captcha")
            || lower.contains("challenge-form")
            || lower.contains("anomaly-modal")
        {
            return SourceParse::Blocked;
        }
        let document = Html::parse_document(html);
        let mut results = Vec::new();
        for item in document.select(&BING_ITEM) {
            let Some(link) = item.select(&BING_LINK).next() else {
                continue;
            };
            let url = decode_bing_result_url(link.value().attr("href").unwrap_or_default());
            let snippet = BING_SNIPPETS
                .iter()
                .find_map(|candidate| {
                    item.select(candidate)
                        .map(element_text)
                        .find(|text| !text.is_empty())
                })
                .unwrap_or_default();
            push_result(&mut results, element_text(link), url, snippet);
            if results.len() >= MAX_SEARCH_RESULTS {
                break;
            }
        }
        if !results.is_empty() {
            SourceParse::Results(results)
        } else if lower.contains("class=\"b_no\"")
            || lower.contains("there are no results for")
            || lower.contains("没有找到")
            || lower.contains("找不到与")
        {
            SourceParse::Empty
        } else {
            SourceParse::Unrecognized
        }
    }
}

fn decode_bing_result_url(raw: &str) -> String {
    if raw.contains("bing.com/ck/a") {
        for pair in raw.split('?').nth(1).unwrap_or_default().split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key != "u" {
                continue;
            }
            let Ok(decoded_param) = urlencoding::decode(value) else {
                break;
            };
            let Some(encoded_url) = decoded_param.strip_prefix("a1") else {
                break;
            };
            if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded_url)
                && let Ok(url) = String::from_utf8(bytes)
            {
                return url;
            }
            break;
        }
    }
    raw.to_string()
}

struct Brave;

static BRAVE_ITEM: LazyLock<Selector> =
    LazyLock::new(|| selector("div.snippet[data-type=\"web\"]"));
static BRAVE_LINK: LazyLock<Selector> = LazyLock::new(|| selector(".result-content a[href]"));
static BRAVE_TITLE: LazyLock<Selector> = LazyLock::new(|| selector(".title"));
static BRAVE_CONTENT: LazyLock<Selector> =
    LazyLock::new(|| selector(".generic-snippet .content, .content"));

impl SearchSource for Brave {
    fn name(&self) -> &'static str {
        "brave"
    }

    fn url(&self, query: &str) -> String {
        format!(
            "https://search.brave.com/search?q={}&source=web",
            urlencoding::encode(query)
        )
    }

    fn parse(&self, html: &str) -> SourceParse {
        let lower = html.to_ascii_lowercase();
        if lower.contains("captcha") && !lower.contains("data-type=\"web\"") {
            return SourceParse::Blocked;
        }
        let document = Html::parse_document(html);
        let mut results = Vec::new();
        for item in document.select(&BRAVE_ITEM) {
            let Some(link) = item.select(&BRAVE_LINK).next() else {
                continue;
            };
            let url = link.value().attr("href").unwrap_or_default().to_string();
            let title = item
                .select(&BRAVE_TITLE)
                .next()
                .map(|title| {
                    title
                        .value()
                        .attr("title")
                        .map(collapse_whitespace)
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| element_text(title))
                })
                .unwrap_or_default();
            let snippet = item
                .select(&BRAVE_CONTENT)
                .next()
                .map(element_text)
                .unwrap_or_default();
            push_result(&mut results, title, url, snippet);
            if results.len() >= MAX_SEARCH_RESULTS {
                break;
            }
        }
        if results.is_empty() {
            SourceParse::Unrecognized
        } else {
            SourceParse::Results(results)
        }
    }
}

/// 来源按顺序尝试：前一个失败（网络、人机验证、页面无法识别、结果不相关）才换下一个。
static DEFAULT_SOURCES: [&dyn SearchSource; 2] = [&Bing, &Brave];

// ══════════════════════════════════════════════════════════════════════════════
// 相关性检查
// ══════════════════════════════════════════════════════════════════════════════

const ASCII_STOPWORDS: [&str; 12] = [
    "vs", "of", "the", "and", "for", "to", "in", "on", "is", "a", "an", "or",
];

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{AC00}'..='\u{D7AF}'
    )
}

/// 关键词拆成可比对的词元：非 CJK 文字取连续的字母数字词（过滤停用词），CJK 取相邻二字组。
fn query_keywords(query: &str) -> Vec<String> {
    let mut keywords = Vec::new();
    let mut word = String::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let flush_word = |word: &mut String, keywords: &mut Vec<String>| {
        if word.chars().count() >= 2 && !ASCII_STOPWORDS.contains(&word.as_str()) {
            keywords.push(std::mem::take(word));
        } else {
            word.clear();
        }
    };
    let flush_cjk = |run: &mut Vec<char>, keywords: &mut Vec<String>| {
        match run.len() {
            0 => {}
            1 => keywords.push(run[0].to_string()),
            _ => {
                for pair in run.windows(2) {
                    keywords.push(pair.iter().collect());
                }
            }
        }
        run.clear();
    };
    for ch in query.chars().flat_map(char::to_lowercase) {
        if is_cjk(ch) {
            flush_word(&mut word, &mut keywords);
            cjk_run.push(ch);
        } else if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+' | '#') {
            flush_cjk(&mut cjk_run, &mut keywords);
            word.push(ch);
        } else {
            flush_word(&mut word, &mut keywords);
            flush_cjk(&mut cjk_run, &mut keywords);
        }
    }
    flush_word(&mut word, &mut keywords);
    flush_cjk(&mut cjk_run, &mut keywords);
    keywords.sort();
    keywords.dedup();
    keywords
}

/// 结果是否与关键词相关：每条结果按「命中的关键词占比」打分，达到三分之一算相关，
/// 相关的结果至少占五分之一才认为这次搜索可信。关键词里没有可比对的词元时无法判断，视为可信。
///
/// 这一步专门挡住「请求成功但内容无关」的静默错误（例如搜索引擎对不认识的参数返回默认首页）。
fn results_match_query(query: &str, results: &[SearchResult]) -> bool {
    let keywords = query_keywords(query);
    if keywords.is_empty() || results.is_empty() {
        return true;
    }
    let relevant = results
        .iter()
        .filter(|result| {
            let haystack =
                format!("{} {} {}", result.title, result.snippet, result.url).to_lowercase();
            let hits = keywords
                .iter()
                .filter(|keyword| haystack.contains(keyword.as_str()))
                .count();
            hits * 3 >= keywords.len()
        })
        .count();
    relevant >= results.len().div_ceil(5).max(1)
}

// ══════════════════════════════════════════════════════════════════════════════
// web_search
// ══════════════════════════════════════════════════════════════════════════════

struct SourceAttempt {
    source: &'static str,
    failure: WebFailure,
    attempts: u32,
}

pub(super) fn execute_web_search(input: &str) -> String {
    let request = parse_json_object(input);
    let query = match required_string_field(
        request.as_ref(),
        "query",
        "web_search",
        "缺少搜索关键词 query",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    search_with_sources(
        &query,
        &DEFAULT_SOURCES,
        &Budget::new(SEARCH_BUDGET, SEARCH_ATTEMPT_TIMEOUT),
    )
}

fn search_with_sources(query: &str, sources: &[&dyn SearchSource], budget: &Budget) -> String {
    let mut failures: Vec<SourceAttempt> = Vec::new();
    for source in sources {
        if budget.remaining() < MIN_ATTEMPT_WINDOW && !failures.is_empty() {
            break;
        }
        let (response, attempts) = match get_with_retry("web_search", &source.url(query), budget) {
            Ok(fetched) => fetched,
            Err(failure) => {
                failures.push(SourceAttempt {
                    source: source.name(),
                    failure: failure.failure,
                    attempts: failure.attempts,
                });
                continue;
            }
        };
        let html = match response.text() {
            Ok(html) => html,
            Err(error) => {
                tracing::warn!(
                    tool = "web_search",
                    source = source.name(),
                    error = %error_chain(&error),
                    "web search response body failed"
                );
                failures.push(SourceAttempt {
                    source: source.name(),
                    failure: classify_request_error(&error),
                    attempts,
                });
                continue;
            }
        };
        match source.parse(&html) {
            SourceParse::Results(results) => {
                if results_match_query(query, &results) {
                    return search_success(query, source.name(), &results);
                }
                failures.push(SourceAttempt {
                    source: source.name(),
                    failure: WebFailure::Irrelevant,
                    attempts,
                });
            }
            SourceParse::Empty => return search_success(query, source.name(), &[]),
            SourceParse::Blocked => failures.push(SourceAttempt {
                source: source.name(),
                failure: WebFailure::Blocked,
                attempts,
            }),
            SourceParse::Unrecognized => failures.push(SourceAttempt {
                source: source.name(),
                failure: WebFailure::Unrecognized,
                attempts,
            }),
        }
    }
    search_failure(&failures)
}

fn search_success(query: &str, source: &str, results: &[SearchResult]) -> String {
    let items = results
        .iter()
        .map(|result| {
            let mut item = json!({
                "title": result.title,
                "url": result.url,
                "snippet": result.snippet,
            });
            if let (Some(published), Some(object)) = (&result.published, item.as_object_mut()) {
                object.insert("published".to_string(), json!(published));
            }
            item
        })
        .collect::<Vec<_>>();
    json!({
        "tool": "web_search",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "query": query,
        "source": source,
        "result_count": items.len(),
        "results": items,
        "summary": format!("搜索 \"{}\" 返回 {} 条结果", query, items.len()),
    })
    .to_string()
}

fn search_failure(failures: &[SourceAttempt]) -> String {
    // 只有「拿到了结果但全部不相关」才是内容问题；其余都是来源不可用。
    let content_problem = !failures.is_empty()
        && failures.iter().all(|attempt| {
            matches!(
                attempt.failure,
                WebFailure::Irrelevant | WebFailure::Unrecognized
            )
        })
        && failures
            .iter()
            .any(|attempt| attempt.failure == WebFailure::Irrelevant);
    let (code, instruction) = if content_problem {
        (
            "web_search_irrelevant_results",
            "搜索来源返回的结果与关键词不相关：请换用更具体或不同语言的关键词，或用 web_fetch 直接访问已知网址；不要用相同关键词重复调用。",
        )
    } else {
        (
            "web_search_unavailable",
            "搜索服务当前不可用，已自动重试并尝试了备用来源：短时间内用相同关键词再次调用不会改变结果，不要重复调用；可用 web_fetch 直接访问已知网址，或在回答里向用户说明暂时无法联网搜索。",
        )
    };
    let summary = failures
        .iter()
        .map(|attempt| {
            format!(
                "{}（{}{}）",
                attempt.source,
                attempt.failure.describe(),
                retried_suffix(attempt.attempts)
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    let details = failures
        .iter()
        .map(|attempt| {
            let mut detail = json!({
                "source": attempt.source,
                "reason": attempt.failure.slug(),
                "message": attempt.failure.describe(),
                "requests": attempt.attempts,
            });
            if let (Some(status), Some(object)) =
                (attempt.failure.http_status(), detail.as_object_mut())
            {
                object.insert("http_status".to_string(), json!(status));
            }
            detail
        })
        .collect::<Vec<_>>();
    ToolFailure::coded("web_search", code, format!("网络搜索失败：{summary}"))
        .instruction(instruction)
        .with("sources", details)
        .into_payload()
}

// ══════════════════════════════════════════════════════════════════════════════
// web_fetch
// ══════════════════════════════════════════════════════════════════════════════

fn content_type_is_readable(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.is_empty()
        || essence.starts_with("text/")
        || essence.ends_with("json")
        || essence.ends_with("xml")
        || essence.ends_with("javascript")
        || essence == "application/x-www-form-urlencoded"
}

fn fetch_failure(failure: &WebFailure, attempts: u32) -> String {
    let error = match failure {
        WebFailure::Status(code) => format!("网页返回 HTTP {code}{}", retried_suffix(attempts)),
        other => format!(
            "网页获取失败：{}{}",
            other.describe(),
            retried_suffix(attempts)
        ),
    };
    let instruction = match failure {
        WebFailure::Status(401 | 403) => {
            "站点拒绝访问（需要登录或禁止抓取）：换用其他来源，不要用相同地址重复调用。"
        }
        WebFailure::Status(404 | 410) => "页面不存在：核对网址，或先用 web_search 找到正确地址。",
        WebFailure::Status(_) => {
            "站点服务异常，已自动重试：不要立刻用相同地址重复调用，可稍后再试或换用其他来源。"
        }
        WebFailure::InvalidUrl => "请提供以 http:// 或 https:// 开头的完整网址。",
        WebFailure::UnsupportedContent(_) => {
            "web_fetch 只能读取网页和文本内容；该地址是二进制或媒体文件，请改用其他方式获取。"
        }
        WebFailure::Dns | WebFailure::Connect | WebFailure::Timeout | WebFailure::Network => {
            "网络暂时不可达，已自动重试：不要用相同地址立刻重复调用；可换用其他来源，或向用户说明暂时无法访问该网站。"
        }
        _ => "请核对网址，或换用其他来源。",
    };
    let mut payload = ToolFailure::new("web_fetch", failure.slug(), error).instruction(instruction);
    if let Some(status) = failure.http_status() {
        payload = payload.with("http_status", status);
    }
    payload.into_payload()
}

pub(super) fn execute_web_fetch(input: &str) -> String {
    let request = parse_json_object(input);
    let url = match required_string_field(request.as_ref(), "url", "web_fetch", "缺少 URL") {
        Ok(value) => value,
        Err(error) => return error,
    };
    match Url::parse(&url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
        _ => return fetch_failure(&WebFailure::InvalidUrl, 0),
    }

    let budget = Budget::new(FETCH_BUDGET, FETCH_ATTEMPT_TIMEOUT);
    let (mut response, attempts) = match get_with_retry("web_fetch", &url, &budget) {
        Ok(fetched) => fetched,
        Err(failure) => return fetch_failure(&failure.failure, failure.attempts),
    };

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if !content_type_is_readable(&content_type) {
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        return fetch_failure(&WebFailure::UnsupportedContent(essence), attempts);
    }

    let mut response_bytes = Vec::new();
    let response_truncated = match response
        .by_ref()
        .take(FETCH_MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut response_bytes)
    {
        Ok(_) => response_bytes.len() as u64 > FETCH_MAX_RESPONSE_BYTES,
        Err(error) => {
            tracing::warn!(
                tool = "web_fetch",
                error = %error_chain(&error),
                "web fetch response body failed"
            );
            let failure = if error.kind() == std::io::ErrorKind::TimedOut {
                WebFailure::Timeout
            } else {
                WebFailure::Network
            };
            return fetch_failure(&failure, attempts);
        }
    };
    if response_truncated {
        response_bytes.truncate(FETCH_MAX_RESPONSE_BYTES as usize);
    }
    let body = decode_web_response_body(&response_bytes, &content_type);

    let content = if content_type.contains("json") {
        format!("```json\n{}\n```", body)
    } else if content_type.contains("text/plain") || content_type.contains("xml") {
        body.clone()
    } else {
        html_to_markdown(&body)
    };

    let (content, content_truncated) = truncate_web_content(content, FETCH_MAX_CONTENT_CHARS);
    let truncated = response_truncated || content_truncated;

    json!({
        "tool": "web_fetch",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "url": url,
        "content_type": content_type,
        "content_length": content.len(),
        "truncated": truncated,
        "content": content,
        "summary": format!("已获取 {} ({} 字符)", url, content.len()),
    })
    .to_string()
}

fn truncate_web_content(content: String, max_chars: usize) -> (String, bool) {
    if content.chars().count() <= max_chars {
        return (content, false);
    }

    let prefix = content.chars().take(max_chars).collect::<String>();
    (
        format!("{prefix}\n\n---\n*[内容已截断至 50,000 字符]*"),
        true,
    )
}

fn decode_web_response_body(bytes: &[u8], content_type: &str) -> String {
    let encoding = content_type
        .split(';')
        .find_map(|part| part.trim().strip_prefix("charset="))
        .map(|label| label.trim_matches(['\"', '\'']))
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (decoded, _, _) = encoding.decode(bytes);
    decoded.into_owned()
}

// ══════════════════════════════════════════════════════════════════════════════
// HTML → Markdown
// ══════════════════════════════════════════════════════════════════════════════

fn html_to_markdown(html: &str) -> String {
    let mut cleaned = extract_main_content(html);
    for pattern in [
        r"(?si)<script[\s\S]*?</script>",
        r"(?si)<style[\s\S]*?</style>",
        r"(?si)<noscript[\s\S]*?</noscript>",
    ] {
        cleaned = regex::Regex::new(pattern)
            .unwrap()
            .replace_all(&cleaned, "")
            .to_string();
    }
    for tag in ["nav", "footer", "header", "aside", "iframe"] {
        let pattern = format!(r"(?si)<{tag}[^>]*>[\s\S]*?</{tag}>");
        cleaned = regex::Regex::new(&pattern)
            .unwrap()
            .replace_all(&cleaned, "")
            .to_string();
    }

    let mut md = cleaned;
    for level in 1..=6 {
        let pattern = format!(r"(?si)<h{level}[^>]*>([\s\S]*?)</h{level}>");
        md = regex::Regex::new(&pattern)
            .unwrap()
            .replace_all(&md, |caps: &regex::Captures| {
                format!(
                    "\n{} {}\n",
                    "#".repeat(level),
                    html_fragment_text(&caps[1]).trim()
                )
            })
            .to_string();
    }
    let md = regex::Regex::new(r"(?si)<pre[^>]*>\s*<code[^>]*>([\s\S]*?)</code>\s*</pre>")
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("\n```\n{}\n```\n", html_fragment_text(&caps[1]).trim())
        });
    let md = regex::Regex::new(r"(?si)<code[^>]*>([\s\S]*?)</code>")
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("`{}`", html_fragment_text(&caps[1]).trim())
        });
    let md = regex::Regex::new(r#"(?si)<a[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#)
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("[{}]({})", html_fragment_text(&caps[2]).trim(), &caps[1])
        });
    let md = regex::Regex::new(r"(?si)<li[^>]*>([\s\S]*?)</li>")
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("\n- {}", html_fragment_text(&caps[1]).trim())
        });
    let md = md
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    let md = md.replace("</p>", "\n\n");
    let md = regex::Regex::new(r"<[^>]+>").unwrap().replace_all(&md, "");
    let md = regex::Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&md, "\n\n");

    html_fragment_text(&md).trim().to_string()
}

fn extract_main_content(html: &str) -> String {
    let patterns = [
        r"(?si)<main[^>]*>([\s\S]*?)</main>",
        r"(?si)<article[^>]*>([\s\S]*?)</article>",
        r#"(?si)<div[^>]+role="main"[^>]*>([\s\S]*?)</div>"#,
        r"(?si)<body[^>]*>([\s\S]*?)</body>",
    ];
    for pat in &patterns {
        if let Some(caps) = regex::Regex::new(pat).unwrap().captures(html) {
            return caps[1].to_string();
        }
    }
    html.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn result(title: &str, snippet: &str, url: &str) -> SearchResult {
        SearchResult {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snippet.to_string(),
            published: None,
        }
    }

    #[test]
    fn bing_parser_extracts_results_decodes_entities_and_splits_the_date() {
        let html = r#"
            <html><body><ol id="b_results">
              <li class="b_algo"><h2><a href="https://www.rust-lang.org/">The <strong>Rust</strong> Language</a></h2>
                <div class="b_caption"><p>Aug 15, 2023&ensp;&#0183;&ensp;Reliable &amp; efficient software.</p></div></li>
              <li class="b_algo"><h2><a href="https://example.com/no-snippet">没有摘要的结果</a></h2></li>
              <li class="b_algo"><h2><a href="https://www.rust-lang.org/">重复地址</a></h2></li>
            </ol></body></html>
        "#;

        let SourceParse::Results(results) = Bing.parse(html) else {
            panic!("应当解析出结果");
        };

        assert_eq!(results.len(), 2, "无摘要的结果保留，重复地址去重");
        assert_eq!(results[0].title, "The Rust Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].snippet, "Reliable & efficient software.");
        assert_eq!(results[0].published.as_deref(), Some("Aug 15, 2023"));
        assert_eq!(results[1].snippet, "");
    }

    #[test]
    fn bing_parser_decodes_tracking_redirect_urls() {
        let target =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("https://example.com/a?b=1");
        let html = format!(
            r#"<li class="b_algo"><h2><a href="https://www.bing.com/ck/a?!&amp;&amp;p=x&amp;u=a1{target}&amp;ntb=1">标题</a></h2><div class="b_caption"><p>摘要</p></div></li>"#
        );

        let SourceParse::Results(results) = Bing.parse(&html) else {
            panic!("应当解析出结果");
        };

        assert_eq!(results[0].url, "https://example.com/a?b=1");
    }

    #[test]
    fn bing_parser_distinguishes_blocked_empty_and_unrecognized_pages() {
        assert!(matches!(
            Bing.parse(r#"<form class="challenge-form"></form>"#),
            SourceParse::Blocked
        ));
        assert!(matches!(
            Bing.parse(r#"<li class="b_no">There are no results for x</li>"#),
            SourceParse::Empty
        ));
        assert!(matches!(
            Bing.parse("<html><body>maintenance</body></html>"),
            SourceParse::Unrecognized
        ));
    }

    #[test]
    fn bing_request_does_not_force_language_or_region() {
        let url = Bing.url("港口物流 自动化 趋势");

        assert!(url.starts_with("https://www.bing.com/search?q="));
        assert!(
            !url.contains("setlang") && !url.contains("cc="),
            "写死地区会让中文关键词返回无关页面: {url}"
        );
    }

    #[test]
    fn brave_parser_extracts_title_link_and_dated_snippet() {
        let html = r#"
            <div class="snippet" data-pos="0" data-type="web"><div class="result-body"><div class="result-content">
              <a href="https://www.reddit.com/r/rust/" class="l1"><div class="title search-snippet-title" title="r/rust: Async Runtimes">r/rust: Async Runtimes</div></a>
              <div class="generic-snippet"><div class="content"><span class="t-secondary">September 22, 2023 -</span> Async basically scales better.</div></div>
            </div></div></div>
            <div class="snippet" data-type="news"><a href="https://news.example/">不是网页结果</a></div>
        "#;

        let SourceParse::Results(results) = Brave.parse(html) else {
            panic!("应当解析出结果");
        };

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "r/rust: Async Runtimes");
        assert_eq!(results[0].url, "https://www.reddit.com/r/rust/");
        assert_eq!(results[0].snippet, "Async basically scales better.");
        assert_eq!(results[0].published.as_deref(), Some("September 22, 2023"));
    }

    #[test]
    fn published_prefix_is_split_for_common_formats() {
        assert_eq!(
            split_published_prefix("2024年3月5日 · 港口自动化加速"),
            (
                Some("2024年3月5日".to_string()),
                "港口自动化加速".to_string()
            )
        );
        assert_eq!(
            split_published_prefix("1 day ago · New release"),
            (Some("1 day ago".to_string()), "New release".to_string())
        );
        assert_eq!(
            split_published_prefix("没有日期的正文 - 含连字符"),
            (None, "没有日期的正文 - 含连字符".to_string())
        );
    }

    #[test]
    fn keywords_use_bigrams_for_cjk_and_words_for_the_rest() {
        let keywords = query_keywords("港口物流 tokio vs async-std");

        assert!(keywords.contains(&"港口".to_string()));
        assert!(keywords.contains(&"物流".to_string()));
        assert!(keywords.contains(&"tokio".to_string()));
        assert!(keywords.contains(&"async-std".to_string()));
        assert!(!keywords.contains(&"vs".to_string()), "停用词不参与比对");
    }

    #[test]
    fn relevance_check_rejects_results_that_ignore_the_query() {
        let junk = vec![
            result(
                "Microsoft——AI、云、生产力",
                "浏览适合家庭或企业的产品，支持自动更新",
                "https://www.microsoft.com/zh-cn",
            ),
            result(
                "Windows 更新助手",
                "手动下载并安装最新的功能更新",
                "https://support.microsoft.com/",
            ),
            result(
                "Microsoft 开发人员",
                "开发人员社区、工具和资源",
                "https://developer.microsoft.com/",
            ),
        ];
        assert!(
            !results_match_query("港口物流 自动化 趋势", &junk),
            "与关键词几乎无关的结果不能当成成功"
        );

        let good = vec![
            result(
                "智慧港口：自动化码头的物流趋势",
                "港口物流自动化正在加速",
                "https://example.com/a",
            ),
            result("无关的页面", "完全不相干", "https://example.com/b"),
            result(
                "港口物流趋势报告",
                "自动化设备渗透率",
                "https://example.com/c",
            ),
        ];
        assert!(results_match_query("港口物流 自动化 趋势", &good));
        assert!(results_match_query(
            "rust async runtime",
            &[result(
                "Tokio: an async runtime for Rust",
                "",
                "https://tokio.rs"
            )]
        ));
        assert!(
            results_match_query("!!!", &junk),
            "没有可比对的词元时无法判断，视为可信"
        );
    }

    #[test]
    fn failure_classes_describe_category_without_internal_details() {
        for failure in [
            WebFailure::Timeout,
            WebFailure::Dns,
            WebFailure::Connect,
            WebFailure::Tls,
            WebFailure::Network,
            WebFailure::Status(503),
        ] {
            let text = failure.describe().to_ascii_lowercase();
            assert!(
                !text.contains("refused") && !text.contains("tcp") && !text.contains("127.0.0.1"),
                "{text}"
            );
        }
        assert!(WebFailure::Status(503).transient());
        assert!(!WebFailure::Status(404).transient());
        assert!(!WebFailure::Blocked.transient());
    }

    #[test]
    fn html_fragment_text_decodes_entities_once_and_strips_tags() {
        assert_eq!(
            html_fragment_text("a&nbsp;<b>b</b> &amp;lt; &#0183;"),
            "a\u{a0}b &lt; ·"
        );
    }

    // ── 搜索来源调度：用本地服务模拟来源，覆盖「换来源 / 结果校验 / 失败汇总」 ──

    use std::{io::Write as _, net::TcpListener};

    /// 每个连接回应一份响应的本地服务；`None` 表示端口已关闭（模拟连不上）。
    fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let handle = std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer);
                stream.write_all(response.as_bytes()).expect("write");
            }
        });
        (url, handle)
    }

    fn closed_port_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        drop(listener);
        url
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// 测试来源：响应体是 JSON 数组（结果列表）、`BLOCK` 或空。
    struct FakeSource {
        name: &'static str,
        url: String,
    }

    impl SearchSource for FakeSource {
        fn name(&self) -> &'static str {
            self.name
        }

        fn url(&self, _query: &str) -> String {
            self.url.clone()
        }

        fn parse(&self, html: &str) -> SourceParse {
            if html.trim() == "BLOCK" {
                return SourceParse::Blocked;
            }
            let Ok(items) = serde_json::from_str::<Vec<Value>>(html) else {
                return SourceParse::Unrecognized;
            };
            SourceParse::Results(
                items
                    .iter()
                    .map(|item| {
                        result(
                            item["title"].as_str().unwrap_or_default(),
                            item["snippet"].as_str().unwrap_or_default(),
                            item["url"].as_str().unwrap_or("https://example.com/"),
                        )
                    })
                    .collect(),
            )
        }
    }

    fn run_search(query: &str, sources: &[&FakeSource]) -> Value {
        let sources = sources
            .iter()
            .map(|source| *source as &dyn SearchSource)
            .collect::<Vec<_>>();
        let output = search_with_sources(
            query,
            &sources,
            &Budget::new(Duration::from_secs(20), Duration::from_secs(5)),
        );
        serde_json::from_str(&output).expect("payload json")
    }

    const GOOD: &str = r#"[{"title":"港口物流自动化趋势","snippet":"智慧港口正在加速","url":"https://example.com/a"}]"#;
    const JUNK: &str =
        r#"[{"title":"Microsoft 首页","snippet":"产品与服务","url":"https://www.microsoft.com/"}]"#;

    #[test]
    fn search_falls_back_to_the_next_source_when_the_first_is_unreachable() {
        let (good_url, server) = serve(vec![ok(GOOD)]);
        let first = FakeSource {
            name: "first",
            url: closed_port_url(),
        };
        let second = FakeSource {
            name: "second",
            url: good_url,
        };

        let payload = run_search("港口物流 自动化 趋势", &[&first, &second]);
        server.join().expect("server");

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["source"], "second");
        assert_eq!(payload["result_count"], 1);
    }

    #[test]
    fn search_never_reports_irrelevant_results_as_success() {
        let (junk_url, junk_server) = serve(vec![ok(JUNK)]);
        let (good_url, good_server) = serve(vec![ok(GOOD)]);
        let first = FakeSource {
            name: "first",
            url: junk_url,
        };
        let second = FakeSource {
            name: "second",
            url: good_url,
        };

        let payload = run_search("港口物流 自动化 趋势", &[&first, &second]);
        junk_server.join().expect("server");
        good_server.join().expect("server");

        assert_eq!(
            payload["source"], "second",
            "第一个来源的无关结果必须被丢弃，改用下一个来源"
        );

        let (only_junk_url, only_junk_server) = serve(vec![ok(JUNK)]);
        let only = FakeSource {
            name: "only",
            url: only_junk_url,
        };
        let payload = run_search("港口物流 自动化 趋势", &[&only]);
        only_junk_server.join().expect("server");

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "web_search_irrelevant_results");
        assert!(
            payload["instruction"]
                .as_str()
                .is_some_and(|text| text.contains("不要用相同关键词重复调用"))
        );
    }

    #[test]
    fn search_failure_lists_every_source_and_tells_the_model_not_to_repeat_the_call() {
        let (blocked_url, server) = serve(vec![ok("BLOCK")]);
        let first = FakeSource {
            name: "first",
            url: closed_port_url(),
        };
        let second = FakeSource {
            name: "second",
            url: blocked_url,
        };

        let payload = run_search("rust async runtime", &[&first, &second]);
        server.join().expect("server");

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "web_search_unavailable");
        let sources = payload["sources"].as_array().expect("sources");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["source"], "first");
        assert_eq!(sources[0]["reason"], "connect_failed");
        assert_eq!(sources[0]["requests"], 3, "暂态故障在访问层重试到上限");
        assert_eq!(sources[1]["reason"], "blocked");
        let text = payload.to_string();
        assert!(
            !text.contains("127.0.0.1") && !text.contains("refused"),
            "失败信息不能暴露内部地址或底层错误: {text}"
        );
        assert!(
            payload["instruction"]
                .as_str()
                .is_some_and(|text| text.contains("不要重复调用"))
        );
    }

    #[test]
    fn truncate_web_content_respects_utf8_character_boundaries() {
        let content = "中".repeat(50_001);

        let (truncated, was_truncated) = truncate_web_content(content, 50_000);

        assert!(was_truncated);
        assert!(truncated.starts_with(&"中".repeat(50_000)));
        assert!(truncated.contains("内容已截断至 50,000 字符"));
    }

    #[test]
    fn web_response_decoder_honors_declared_legacy_charset() {
        let (encoded, _, _) = encoding_rs::BIG5.encode("繁體中文");

        let decoded = decode_web_response_body(&encoded, "text/plain; charset=big5");

        assert_eq!(decoded, "繁體中文");
    }

    #[test]
    fn readable_content_types_exclude_binary_and_media() {
        assert!(content_type_is_readable(""));
        assert!(content_type_is_readable("text/html; charset=utf-8"));
        assert!(content_type_is_readable("application/ld+json"));
        assert!(content_type_is_readable("application/atom+xml"));
        assert!(!content_type_is_readable("application/pdf"));
        assert!(!content_type_is_readable("image/png"));
        assert!(!content_type_is_readable("application/octet-stream"));
    }
}
