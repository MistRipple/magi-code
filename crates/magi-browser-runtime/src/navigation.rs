use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BrowserNavigationUrlError {
    #[error("browser navigation URL must use http or https, or be about:blank")]
    UnsupportedScheme,
    #[error("browser navigation URL is invalid")]
    InvalidUrl,
    #[error("browser navigation URL must not contain user credentials")]
    UserCredentialsNotAllowed,
    #[error("browser navigation URL targets a blocked metadata endpoint")]
    BlockedMetadataEndpoint,
}

/// 将 Browser Host 的页面状态收敛到可恢复的 URL 边界。
///
/// Chromium 在网络失败或内部错误页时可能返回 `chrome-error://` 等内部
/// scheme。它们只代表当前渲染失败，不能作为下一次恢复导航的输入；统一
/// 降级到空白页，避免重启后反复恢复同一个无效 URL。
pub fn normalize_browser_page_state(
    url: String,
    origin: Option<String>,
    title: String,
) -> (String, Option<String>, String) {
    if validate_browser_navigation_url(&url).is_ok() {
        return (url, origin, title);
    }
    ("about:blank".to_string(), None, String::new())
}

/// 校验所有进入 Browser Host 的 URL。浏览器不做逐 Origin 授权，但固定阻止危险
/// scheme、URL 凭据和云元数据地址，避免本机网络边界被网页导航绕过。
pub fn validate_browser_navigation_url(raw_url: &str) -> Result<(), BrowserNavigationUrlError> {
    let url = Url::parse(raw_url.trim()).map_err(|_| BrowserNavigationUrlError::InvalidUrl)?;
    if url.as_str() == "about:blank" {
        return Ok(());
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(BrowserNavigationUrlError::UnsupportedScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(BrowserNavigationUrlError::UserCredentialsNotAllowed);
    }
    let Some(host) = url.host_str() else {
        return Err(BrowserNavigationUrlError::InvalidUrl);
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host == "metadata.google.internal" || host.starts_with("169.254.") {
        return Err(BrowserNavigationUrlError::BlockedMetadataEndpoint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BrowserNavigationUrlError, validate_browser_navigation_url};

    #[test]
    fn accepts_http_https_and_about_blank() {
        for url in [
            "about:blank",
            "https://example.com/path",
            "http://127.0.0.1:38123/web.html",
        ] {
            assert!(validate_browser_navigation_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn rejects_dangerous_schemes() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "chrome://settings",
        ] {
            assert_eq!(
                validate_browser_navigation_url(url),
                Err(BrowserNavigationUrlError::UnsupportedScheme),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_credentials_and_malformed_urls() {
        assert_eq!(
            validate_browser_navigation_url("https://user:password@example.com"),
            Err(BrowserNavigationUrlError::UserCredentialsNotAllowed)
        );
        assert_eq!(
            validate_browser_navigation_url("https://"),
            Err(BrowserNavigationUrlError::InvalidUrl)
        );
    }

    #[test]
    fn rejects_cloud_metadata_endpoints() {
        for url in [
            "http://169.254.169.254/latest/meta-data",
            "https://169.254.10.20/path",
            "http://metadata.google.internal/computeMetadata/v1",
            "http://metadata.google.internal./computeMetadata/v1",
        ] {
            assert_eq!(
                validate_browser_navigation_url(url),
                Err(BrowserNavigationUrlError::BlockedMetadataEndpoint),
                "{url}"
            );
        }
    }
}
