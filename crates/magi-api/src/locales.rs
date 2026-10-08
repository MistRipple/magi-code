//! 界面语言的受支持集合。前端注册表 `web/src/i18n/locales.ts` 的 `LOCALES` 必须与这里一致，
//! 由下方测试读取该文件校验；新增语言时两处同改。

pub const DEFAULT_LOCALE: &str = "zh-CN";

pub const SUPPORTED_LOCALES: &[&str] = &["zh-CN", "en-US"];

pub fn is_supported_locale(value: &str) -> bool {
    SUPPORTED_LOCALES.contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_locales_match_web_registry() {
        let registry = include_str!("../../../web/src/i18n/locales.ts");
        let block = registry
            .split("export const LOCALES = [")
            .nth(1)
            .and_then(|rest| rest.split("] as const;").next())
            .expect("web locale registry must declare LOCALES");
        let web_codes: Vec<&str> = block
            .lines()
            .filter_map(|line| line.split("code: '").nth(1))
            .filter_map(|rest| rest.split('\'').next())
            .collect();
        assert_eq!(web_codes, SUPPORTED_LOCALES);
        assert!(is_supported_locale(DEFAULT_LOCALE));
    }
}
