//! GUI 图片的格式与内容准入。原始字节（包括 SVG 矢量）保持不变。
//! 不决定文件权限、图标体积或模型传输格式，这些仍由各自调用边界拥有。
use std::path::Path;

/// 图片响应必须使用此隔离策略，即使用户将 SVG 地址直接作为文档打开。
/// 内联样式和嵌入图片/字体可显示，脚本、网络子资源、表单和文档导航不可执行。
pub const IMAGE_CONTENT_SECURITY_POLICY: &str = "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; base-uri 'none'; form-action 'none'";

pub fn image_content_type(path: impl AsRef<Path>) -> Option<&'static str> {
    match path
        .as_ref()
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// 核对声明与文件格式。位图解码由浏览器负责，不能把这个检查当作完整解码。
/// SVG 经有界 XML 解析，图片消费者只能用 img，不能注入可信 DOM。
pub fn validate_image(path: impl AsRef<Path>, bytes: &[u8]) -> Result<&'static str, &'static str> {
    let mime = image_content_type(path).ok_or("不支持的图片格式")?;
    let matches = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "image/bmp" => bytes.starts_with(b"BM"),
        "image/x-icon" => bytes.starts_with(&[0, 0, 1, 0]),
        "image/avif" => avif_brand(bytes),
        "image/svg+xml" => {
            validate_svg(bytes)?;
            true
        }
        _ => false,
    };
    if matches {
        Ok(mime)
    } else {
        Err("图片文件内容与其类型不符")
    }
}

fn avif_brand(bytes: &[u8]) -> bool {
    // ISO BMFF ftyp：只看完整的 major/compatible brand，不能匹配 minor version
    // 或后续 box 的任意子串；compatible brands 不限于前 32 字节。
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    if size < 16 || size > bytes.len() || (size - 16) % 4 != 0 {
        return false;
    }
    let supported = |brand: &[u8]| brand == b"avif" || brand == b"avis";
    supported(&bytes[8..12]) || bytes[16..size].chunks_exact(4).any(supported)
}

fn validate_svg(bytes: &[u8]) -> Result<(), &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "SVG 必须是 UTF-8 XML")?;
    let document = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
            ..Default::default()
        },
    )
    .map_err(|_| "SVG XML 无效、包含 DTD 或节点过多")?;
    let root = document.root_element().tag_name();
    if root.name() != "svg" || root.namespace() != Some("http://www.w3.org/2000/svg") {
        return Err("SVG 缺少有效的 svg 根元素及命名空间");
    }
    for node in document.descendants() {
        if node.is_pi()
            || (node.is_element() && node.tag_name().name().eq_ignore_ascii_case("script"))
        {
            return Err("SVG 图片不能包含脚本或处理指令");
        }
        for attribute in node.attributes() {
            let name = attribute.name().to_ascii_lowercase();
            let value = attribute.value().trim();
            if name.starts_with("on")
                || (name == "href"
                    && !value.starts_with('#')
                    && !value.starts_with("data:image/")
                    && !value.starts_with("data:font/"))
            {
                return Err("SVG 图片不能包含事件或外部引用");
            }
        }
    }
    // 渐变、滤镜、mask、style、动画和 foreignObject 中的静态排版保留。
    // CSS URL 与嵌入文档不在此手写解析，统一由 img 上下文和响应 CSP 限制。
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_formats_have_one_case_insensitive_mime_catalog() {
        for (extension, mime, bytes) in [
            ("PNG", "image/png", b"\x89PNG\r\n\x1a\n".as_slice()),
            ("jpeg", "image/jpeg", &[0xff, 0xd8, 0xff]),
            ("gif", "image/gif", b"GIF89a"),
            ("webp", "image/webp", b"RIFF\0\0\0\0WEBPVP8 "),
            ("bmp", "image/bmp", b"BM"),
            ("ico", "image/x-icon", &[0, 0, 1, 0]),
        ] {
            assert_eq!(
                validate_image(format!("image.{extension}"), bytes),
                Ok(mime)
            );
            assert!(validate_image(format!("image.{extension}"), b"<html/>").is_err());
        }
        assert_eq!(image_content_type("document.html"), None);
    }

    #[test]
    fn svg_keeps_vector_features_and_rejects_active_or_invalid_xml() {
        let svg = br##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><style>.shape{fill:url(#g)}</style><defs><linearGradient id="g"><stop stop-color="red"/></linearGradient></defs><rect class="shape" width="32" height="32"/><foreignObject width="32" height="32"><div xmlns="http://www.w3.org/1999/xhtml">text</div></foreignObject></svg>"##;
        assert_eq!(validate_image("logo.SVG", svg), Ok("image/svg+xml"));
        for fragment in [
            "<script/>",
            "<rect onload='run()'/>",
            "<image href='https://example.test/x'/>",
            "<use href='javascript:run()'/>",
            "<?xml-stylesheet href='x'?>",
        ] {
            let svg = format!("<svg xmlns='http://www.w3.org/2000/svg'>{fragment}</svg>");
            assert!(
                validate_image("x.svg", svg.as_bytes()).is_err(),
                "{fragment}"
            );
        }
        for svg in [
            "<svg/>",
            "<html/>",
            "<!DOCTYPE svg><svg xmlns='http://www.w3.org/2000/svg'/>",
            "<svg xmlns='http://www.w3.org/2000/svg'>",
        ] {
            assert!(validate_image("x.svg", svg.as_bytes()).is_err());
        }
    }

    #[test]
    fn avif_recognizes_aligned_compatible_brands_and_rejects_spoofs() {
        let valid = b"\0\0\0\x28ftypmif1\0\0\0\0mif1miafMA1BMA1Amif1avif";
        assert_eq!(validate_image("x.avif", valid), Ok("image/avif"));
        assert!(validate_image("x.avif", b"\0\0\0\x10ftypmif1avif").is_err());
        assert!(validate_image("x.avif", &valid[..20]).is_err());
    }
}
