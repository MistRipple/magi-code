use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenContext {
    Definition,
    Usage,
    Comment,
    String,
    Import,
}

#[derive(Clone, Debug)]
pub struct TokenWithPosition {
    pub token: String,
    pub line: usize,
    pub column: usize,
    pub context: TokenContext,
}

#[derive(Clone, Debug)]
pub struct FileTokenResult {
    pub file_path: String,
    pub tokens: Vec<TokenWithPosition>,
    pub frequencies: Vec<(String, usize)>,
    pub total_tokens: usize,
}

fn is_code_stop_word(word: &str) -> bool {
    matches!(
        word,
        "const"
            | "let"
            | "var"
            | "function"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "break"
            | "continue"
            | "new"
            | "delete"
            | "typeof"
            | "instanceof"
            | "void"
            | "null"
            | "undefined"
            | "true"
            | "false"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "yield"
            | "static"
            | "private"
            | "protected"
            | "public"
            | "readonly"
            | "declare"
            | "module"
            | "require"
            | "from"
            | "as"
            | "default"
            | "super"
            | "fn"
            | "pub"
            | "mut"
            | "ref"
            | "self"
            | "impl"
            | "mod"
            | "use"
            | "crate"
            | "where"
            | "match"
            | "loop"
            | "move"
            | "unsafe"
            | "def"
            | "elif"
            | "pass"
            | "with"
            | "lambda"
            | "nonlocal"
            | "global"
            | "func"
            | "defer"
            | "go"
            | "select"
            | "chan"
            | "range"
    )
}

fn is_english_stop_word(word: &str) -> bool {
    matches!(
        word,
        "the"
            | "a"
            | "an"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "being"
            | "have"
            | "has"
            | "had"
            | "do"
            | "does"
            | "did"
            | "will"
            | "would"
            | "could"
            | "should"
            | "may"
            | "might"
            | "shall"
            | "can"
            | "of"
            | "in"
            | "to"
            | "for"
            | "with"
            | "on"
            | "at"
            | "by"
            | "this"
            | "that"
            | "these"
            | "those"
            | "it"
            | "its"
            | "not"
            | "no"
            | "or"
            | "and"
            | "but"
            | "so"
            | "then"
            | "than"
            | "also"
            | "just"
    )
}

fn is_structural_keyword(word: &str) -> bool {
    matches!(
        word,
        "import"
            | "export"
            | "async"
            | "await"
            | "interface"
            | "type"
            | "class"
            | "enum"
            | "extends"
            | "implements"
            | "abstract"
            | "struct"
            | "trait"
    )
}

fn is_chinese_char(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

fn has_chinese(text: &str) -> bool {
    text.chars().any(is_chinese_char)
}

fn is_pure_number(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

#[derive(Clone, Debug, Default)]
pub struct CodeTokenizer;

impl CodeTokenizer {
    pub fn new() -> Self {
        Self
    }

    pub fn tokenize_file(&self, file_path: &str, content: &str) -> FileTokenResult {
        let mut tokens = Vec::new();
        let mut freq_map = std::collections::HashMap::<String, usize>::new();
        let mut in_block_comment = false;

        for (line_idx, line) in content.lines().enumerate() {
            let (context, new_block_state) = self.detect_line_context(line, in_block_comment);
            in_block_comment = new_block_state;

            for tok in self.tokenize_line(line, line_idx, context) {
                *freq_map.entry(tok.token.clone()).or_insert(0) += 1;
                tokens.push(tok);
            }
        }

        let total_tokens = tokens.len();
        let mut frequencies: Vec<(String, usize)> = freq_map.into_iter().collect();
        frequencies.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        FileTokenResult {
            file_path: file_path.to_string(),
            tokens,
            frequencies,
            total_tokens,
        }
    }

    pub fn tokenize_query(&self, query: &str) -> Vec<String> {
        let mut tokens = BTreeSet::new();

        for tok in self.extract_chinese_tokens(query) {
            tokens.insert(tok);
        }

        for word in extract_identifiers(query) {
            for sub in self.split_identifier(&word) {
                if self.is_valid_token(&sub) {
                    tokens.insert(sub);
                }
            }
        }

        tokens.into_iter().collect()
    }

    pub fn split_identifier(&self, identifier: &str) -> Vec<String> {
        if identifier.len() < 2 {
            return Vec::new();
        }

        let mut tokens = BTreeSet::new();
        let lower = identifier.to_ascii_lowercase();
        tokens.insert(lower.clone());

        if identifier.contains('_') || identifier.contains('-') {
            let parts: Vec<&str> = identifier
                .split(|c: char| c == '_' || c == '-')
                .filter(|p| p.len() >= 2)
                .collect();
            for part in &parts {
                tokens.insert(part.to_ascii_lowercase());
                for camel_part in split_camel_case(part) {
                    let lp = camel_part.to_ascii_lowercase();
                    if lp.len() >= 2 {
                        tokens.insert(lp);
                    }
                }
            }
            return tokens.into_iter().collect();
        }

        for part in split_camel_case(identifier) {
            let lp = part.to_ascii_lowercase();
            if lp.len() >= 2 {
                tokens.insert(lp);
            }
        }

        tokens.into_iter().collect()
    }

    fn detect_line_context(&self, line: &str, in_block_comment: bool) -> (TokenContext, bool) {
        let trimmed = line.trim();

        if in_block_comment {
            if trimmed.contains("*/") {
                return (TokenContext::Comment, false);
            }
            return (TokenContext::Comment, true);
        }

        if trimmed.starts_with("/*") && !trimmed.contains("*/") {
            return (TokenContext::Comment, true);
        }
        if trimmed.starts_with("/*") && trimmed.contains("*/") {
            return (TokenContext::Comment, false);
        }
        if trimmed.contains("/*") && !trimmed.contains("*/") {
            let ctx = self.detect_inline_context(trimmed);
            return (ctx, true);
        }

        if trimmed.starts_with("import ")
            || trimmed.starts_with("import{")
            || trimmed.starts_with("use ")
            || trimmed.starts_with("from ")
        {
            return (TokenContext::Import, false);
        }

        if trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("*")
            || trimmed.starts_with("/**")
        {
            return (TokenContext::Comment, false);
        }

        if is_definition_line(trimmed) {
            return (TokenContext::Definition, false);
        }

        (TokenContext::Usage, false)
    }

    fn detect_inline_context(&self, trimmed: &str) -> TokenContext {
        if trimmed.starts_with("import ")
            || trimmed.starts_with("import{")
            || trimmed.starts_with("use ")
        {
            return TokenContext::Import;
        }
        if is_definition_line(trimmed) {
            return TokenContext::Definition;
        }
        TokenContext::Usage
    }

    fn tokenize_line(
        &self,
        line: &str,
        line_idx: usize,
        context: TokenContext,
    ) -> Vec<TokenWithPosition> {
        let mut result = Vec::new();

        if has_chinese(line) {
            for tok in self.extract_chinese_tokens(line) {
                let col = line.find(&tok).unwrap_or(0);
                result.push(TokenWithPosition {
                    token: tok,
                    line: line_idx,
                    column: col,
                    context,
                });
            }
        }

        for (col, word) in extract_identifier_positions(line) {
            for sub in self.split_identifier(&word) {
                if self.is_valid_token(&sub) {
                    result.push(TokenWithPosition {
                        token: sub,
                        line: line_idx,
                        column: col,
                        context,
                    });
                }
            }
        }

        result
    }

    fn extract_chinese_tokens(&self, text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let segments = extract_chinese_segments(text);

        for seg in &segments {
            let chars: Vec<char> = seg.chars().collect();
            if chars.len() >= 2 {
                tokens.push(seg.clone());
            }
            // bigram
            for i in 0..chars.len().saturating_sub(1) {
                tokens.push(chars[i..i + 2].iter().collect());
            }
            // trigram
            for i in 0..chars.len().saturating_sub(2) {
                tokens.push(chars[i..i + 3].iter().collect());
            }
        }

        tokens
    }

    fn is_valid_token(&self, token: &str) -> bool {
        if token.len() < 2 {
            return false;
        }
        if is_pure_number(token) {
            return false;
        }
        if has_chinese(token) {
            return true;
        }
        let lower = token.to_ascii_lowercase();
        if is_structural_keyword(&lower) {
            return true;
        }
        if is_code_stop_word(&lower) {
            return false;
        }
        if is_english_stop_word(&lower) {
            return false;
        }
        true
    }
}

fn is_definition_line(trimmed: &str) -> bool {
    let def_prefixes = [
        "export ",
        "class ",
        "interface ",
        "type ",
        "enum ",
        "function ",
        "const ",
        "let ",
        "var ",
        "pub fn ",
        "fn ",
        "pub struct ",
        "struct ",
        "pub enum ",
        "pub trait ",
        "trait ",
        "impl ",
        "pub type ",
        "pub const ",
        "pub static ",
        "static ",
        "def ",
        "func ",
    ];
    for prefix in &def_prefixes {
        if trimmed.starts_with(prefix) {
            return true;
        }
    }
    if trimmed.starts_with("export ") {
        let rest = trimmed.trim_start_matches("export ");
        let rest = rest.trim_start_matches("default ");
        for prefix in &def_prefixes {
            if rest.starts_with(prefix) {
                return true;
            }
        }
    }
    false
}

fn split_camel_case(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut start = 0;

    for i in 1..chars.len() {
        let prev = chars[i - 1];
        let curr = chars[i];
        let split = (prev.is_ascii_lowercase() && curr.is_ascii_uppercase())
            || (i + 1 < chars.len()
                && prev.is_ascii_uppercase()
                && curr.is_ascii_uppercase()
                && chars[i + 1].is_ascii_lowercase());

        if split {
            let part: String = chars[start..i].iter().collect();
            if !part.is_empty() {
                parts.push(part);
            }
            start = i;
        }
    }

    let last: String = chars[start..].iter().collect();
    if !last.is_empty() {
        parts.push(last);
    }

    parts
}

fn extract_identifiers(text: &str) -> Vec<String> {
    extract_identifier_positions(text)
        .into_iter()
        .map(|(_, word)| word)
        .collect()
}

fn extract_identifier_positions(text: &str) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            result.push((start, word));
        } else {
            i += 1;
        }
    }

    result
}

fn extract_chinese_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();

    for c in text.chars() {
        if is_chinese_char(c) {
            current.push(c);
        } else if !current.is_empty() {
            segments.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        segments.push(current);
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_camel_case() {
        assert_eq!(
            split_camel_case("getProjectContext"),
            vec!["get", "Project", "Context"]
        );
        assert_eq!(split_camel_case("HTMLParser"), vec!["HTML", "Parser"]);
    }

    #[test]
    fn test_split_identifier() {
        let t = CodeTokenizer::new();
        let result = t.split_identifier("getProjectContext");
        assert!(result.contains(&"getprojectcontext".to_string()));
        assert!(result.contains(&"get".to_string()));
        assert!(result.contains(&"project".to_string()));
        assert!(result.contains(&"context".to_string()));
    }

    #[test]
    fn test_split_snake_case() {
        let t = CodeTokenizer::new();
        let result = t.split_identifier("snake_case_var");
        assert!(result.contains(&"snake_case_var".to_string()));
        assert!(result.contains(&"snake".to_string()));
        assert!(result.contains(&"case".to_string()));
        assert!(result.contains(&"var".to_string()));
    }

    #[test]
    fn test_tokenize_query_with_chinese() {
        let t = CodeTokenizer::new();
        let result = t.tokenize_query("搜索引擎");
        assert!(!result.is_empty());
        assert!(result.contains(&"搜索引擎".to_string()));
        assert!(result.contains(&"搜索".to_string()));
        assert!(result.contains(&"索引".to_string()));
        assert!(result.contains(&"引擎".to_string()));
    }

    #[test]
    fn test_stop_words_filtered() {
        let t = CodeTokenizer::new();
        let result = t.tokenize_query("const function return");
        assert!(result.is_empty());
    }

    #[test]
    fn test_structural_keywords_kept() {
        let t = CodeTokenizer::new();
        let result = t.tokenize_query("import export class");
        assert!(result.contains(&"import".to_string()));
        assert!(result.contains(&"export".to_string()));
        assert!(result.contains(&"class".to_string()));
    }
}
