//! GPT Web 回复里的图片。
//!
//! ChatGPT 生成的图是 `blob:` 地址，只在网页里有效：站点适配层把它们还原成 Markdown 图片引用，
//! 收口时由客户端按地址把字节读出来，交给宿主保存到会话所属项目，再把引用改写成项目内的
//! 相对路径。流式阶段不显示这些引用（此时它们在 Magi 里还不可用）。

use std::ops::Range;

/// 一张待保存的图片。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebImage {
    pub mime: String,
    pub bytes: Vec<u8>,
    /// 页面上的说明文字（如「已生成图像 1」）。
    pub alt: String,
}

/// 宿主提供的图片落地：保存到会话所属项目，返回 Markdown 里引用用的项目相对路径。
pub trait WebImageSink: Send + Sync {
    fn store(&self, session_id: &str, project_id: &str, image: WebImage) -> Result<String, String>;
}

/// 回复文本里一处需要由页面提供字节的图片引用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageImageRef {
    /// 整个 `![alt](src)` 在文本中的字节范围。
    pub range: Range<usize>,
    pub alt: String,
    pub src: String,
}

/// 图片只在页面里可读的来源：`blob:` 和 ChatGPT 自己的地址。其他公开地址原样保留。
fn is_page_only_source(src: &str) -> bool {
    src.starts_with("blob:") || src.starts_with("https://chatgpt.com/")
}

/// 找出文本里的页面图片引用（`![alt](src)`，alt 不含 `]`，src 不含空白和括号）。
pub fn page_image_refs(text: &str) -> Vec<PageImageRef> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find("![") {
        let start = cursor + relative;
        cursor = start + 2;
        let Some(alt_end) = text[cursor..].find("](") else {
            break;
        };
        let alt = &text[cursor..cursor + alt_end];
        if alt.contains(']') || alt.contains('\n') {
            continue;
        }
        let src_start = cursor + alt_end + 2;
        let Some(src_len) = text[src_start..].find(')') else {
            break;
        };
        let src = &text[src_start..src_start + src_len];
        if src.chars().any(char::is_whitespace) || src.contains('(') || !is_page_only_source(src) {
            continue;
        }
        let end = src_start + src_len + 1;
        found.push(PageImageRef {
            range: start..end,
            alt: alt.to_string(),
            src: src.to_string(),
        });
        cursor = end;
    }
    found
}

/// 用 `replace` 的结果替换文本里的每个页面图片引用；返回空串的引用被整段删除。
pub fn rewrite_page_images(text: &str, mut replace: impl FnMut(&PageImageRef) -> String) -> String {
    let refs = page_image_refs(text);
    if refs.is_empty() {
        return text.to_string();
    }
    let mut output = String::with_capacity(text.len());
    let mut last = 0;
    for image in &refs {
        output.push_str(&text[last..image.range.start]);
        output.push_str(&replace(image));
        last = image.range.end;
    }
    output.push_str(&text[last..]);
    collapse_blank_runs(&output)
}

/// 去掉页面图片引用（流式阶段：图片此时在 Magi 里还不可用）。
pub fn strip_page_images(text: &str) -> String {
    rewrite_page_images(text, |_| String::new())
}

fn collapse_blank_runs(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut newlines = 0usize;
    for character in text.chars() {
        if character == '\n' {
            newlines += 1;
            if newlines <= 2 {
                output.push('\n');
            }
        } else {
            newlines = 0;
            output.push(character);
        }
    }
    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_only_images_are_found_and_public_ones_are_left_alone() {
        let text = "前 ![已生成图像 1](blob:https://chatgpt.com/aaa) 中 ![公开](https://example.com/a.png) ![x](blob:https://chatgpt.com/bbb)";
        let refs = page_image_refs(text);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].alt, "已生成图像 1");
        assert_eq!(refs[0].src, "blob:https://chatgpt.com/aaa");
        assert_eq!(refs[1].src, "blob:https://chatgpt.com/bbb");
    }

    #[test]
    fn stripping_keeps_the_text_and_collapses_the_gap_left_by_images() {
        let text = "说明\n\n![图 1](blob:https://chatgpt.com/aaa)\n\n![图 2](blob:https://chatgpt.com/bbb)";
        assert_eq!(strip_page_images(text), "说明");
        assert_eq!(strip_page_images("![图](blob:https://chatgpt.com/aaa)"), "");
        assert_eq!(strip_page_images("没有图片"), "没有图片");
    }

    #[test]
    fn rewriting_replaces_each_reference_in_order() {
        let text = "![a](blob:https://chatgpt.com/1)\n\n![b](blob:https://chatgpt.com/2)";
        let mut index = 0;
        let output = rewrite_page_images(text, |image| {
            index += 1;
            format!("![{}](generated-images/{index}.png)", image.alt)
        });
        assert_eq!(
            output,
            "![a](generated-images/1.png)\n\n![b](generated-images/2.png)"
        );
    }

    #[test]
    fn malformed_references_do_not_panic_on_multibyte_text() {
        assert!(page_image_refs("![图片](blob:").is_empty());
        assert!(page_image_refs("![一二三").is_empty());
        assert!(page_image_refs("![](中文)").is_empty());
    }
}
