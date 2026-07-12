/// 对纯文本进行稳定、语言感知的 token 粗估。
///
/// CJK 字符按一个 token 计算；其余非空白字符按每四个字符一个 token 计算。
/// 该估算不替代模型返回的真实 usage，只用于请求前的上下文预算与水位判断。
pub fn estimate_text_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut cjk = 0usize;
    let mut other = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if is_cjk(ch) {
            cjk += 1;
        } else {
            other += 1;
        }
    }

    cjk + other.div_ceil(4)
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{ac00}'..='\u{d7af}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_cjk_and_ascii_consistently() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("你好"), 2);
        assert_eq!(estimate_text_tokens("hello world"), 3);
        assert_eq!(estimate_text_tokens("abc中文"), 3);
    }
}
