use serde_json::{json, Value};

struct DiagnosticPattern {
    pattern: &'static str,
    severity: &'static str,
    message: &'static str,
    category: &'static str,
}

const PREHOST_DIAGNOSTIC_PATTERNS: &[DiagnosticPattern] = &[
    DiagnosticPattern {
        pattern: "FIXME",
        severity: "warning",
        message: "FIXME marker detected",
        category: "marker",
    },
    DiagnosticPattern {
        pattern: "TODO",
        severity: "info",
        message: "TODO marker detected",
        category: "marker",
    },
    DiagnosticPattern {
        pattern: "HACK",
        severity: "warning",
        message: "HACK marker detected",
        category: "marker",
    },
    DiagnosticPattern {
        pattern: "XXX",
        severity: "warning",
        message: "XXX marker detected",
        category: "marker",
    },
    DiagnosticPattern {
        pattern: "unwrap()",
        severity: "warning",
        message: "unwrap() call detected — consider explicit error handling",
        category: "error_handling",
    },
    DiagnosticPattern {
        pattern: ".expect(",
        severity: "info",
        message: "expect() call detected — verify panic message is descriptive",
        category: "error_handling",
    },
    DiagnosticPattern {
        pattern: "unsafe ",
        severity: "warning",
        message: "unsafe block or function detected",
        category: "safety",
    },
    DiagnosticPattern {
        pattern: "panic!(",
        severity: "error",
        message: "explicit panic!() detected",
        category: "safety",
    },
    DiagnosticPattern {
        pattern: "unimplemented!()",
        severity: "error",
        message: "unimplemented!() detected",
        category: "completeness",
    },
    DiagnosticPattern {
        pattern: "todo!()",
        severity: "warning",
        message: "todo!() macro detected",
        category: "completeness",
    },
    DiagnosticPattern {
        pattern: "#[allow(",
        severity: "info",
        message: "#[allow(..)] attribute detected — verify suppression is intentional",
        category: "lint",
    },
    DiagnosticPattern {
        pattern: "#[deprecated",
        severity: "info",
        message: "deprecated item detected",
        category: "lifecycle",
    },
];

pub(super) fn collect_prehost_diagnostics(content: &str) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line_no = index + 1;
        for pattern in PREHOST_DIAGNOSTIC_PATTERNS {
            if line.contains(pattern.pattern) {
                diagnostics.push(json!({
                    "severity": pattern.severity,
                    "message": pattern.message,
                    "line": line_no,
                    "category": pattern.category,
                    "source": "vscode-prehost-static-scan",
                }));
            }
        }
    }
    diagnostics
}

struct SymbolPattern {
    prefix: &'static str,
    kind: &'static str,
    visibility: &'static str,
}

const PREHOST_SYMBOL_PATTERNS: &[SymbolPattern] = &[
    SymbolPattern {
        prefix: "pub struct ",
        kind: "struct",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "struct ",
        kind: "struct",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub enum ",
        kind: "enum",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "enum ",
        kind: "enum",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub fn ",
        kind: "function",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "fn ",
        kind: "function",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub mod ",
        kind: "module",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "mod ",
        kind: "module",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub trait ",
        kind: "trait",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "trait ",
        kind: "trait",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub const ",
        kind: "constant",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "const ",
        kind: "constant",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub static ",
        kind: "static",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "static ",
        kind: "static",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub type ",
        kind: "type_alias",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "type ",
        kind: "type_alias",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "pub use ",
        kind: "use_reexport",
        visibility: "public",
    },
    SymbolPattern {
        prefix: "macro_rules! ",
        kind: "macro",
        visibility: "private",
    },
    SymbolPattern {
        prefix: "impl ",
        kind: "impl",
        visibility: "inherent",
    },
];

pub(super) fn collect_prehost_symbols(content: &str) -> Vec<Value> {
    content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim_start();
            PREHOST_SYMBOL_PATTERNS.iter().find_map(|pattern| {
                trimmed.strip_prefix(pattern.prefix).and_then(|rest| {
                    let name = rest
                        .chars()
                        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                        .collect::<String>();
                    if name.is_empty() {
                        None
                    } else {
                        Some(json!({
                            "name": name,
                            "kind": pattern.kind,
                            "visibility": pattern.visibility,
                            "line": index + 1,
                            "source": "vscode-prehost-symbol-scan",
                        }))
                    }
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_detects_all_pattern_categories() {
        let content = r#"
fn example() {
    // FIXME: fix this
    // TODO: do this later
    // HACK: workaround
    // XXX: attention needed
    let value = result.unwrap();
    let other = result.expect("should not fail");
    unsafe { ptr::read(addr) }
    panic!("critical failure");
    unimplemented!()
    todo!()
    #[allow(unused)]
    #[deprecated]
}
"#;
        let diagnostics = collect_prehost_diagnostics(content);

        let categories: Vec<&str> = diagnostics
            .iter()
            .map(|d| d["category"].as_str().expect("category should be a string"))
            .collect();
        assert!(categories.contains(&"marker"), "should detect marker category");
        assert!(
            categories.contains(&"error_handling"),
            "should detect error_handling category"
        );
        assert!(categories.contains(&"safety"), "should detect safety category");
        assert!(
            categories.contains(&"completeness"),
            "should detect completeness category"
        );
        assert!(categories.contains(&"lint"), "should detect lint category");
        assert!(
            categories.contains(&"lifecycle"),
            "should detect lifecycle category"
        );

        let fixme = diagnostics
            .iter()
            .find(|d| d["message"].as_str().expect("message should be a string").contains("FIXME"))
            .expect("should find FIXME");
        assert_eq!(fixme["severity"], "warning");

        let panic = diagnostics
            .iter()
            .find(|d| d["message"].as_str().expect("message should be a string").contains("panic!()"))
            .expect("should find panic");
        assert_eq!(panic["severity"], "error");

        let todo_marker = diagnostics
            .iter()
            .find(|d| d["message"] == "TODO marker detected")
            .expect("should find TODO marker");
        assert_eq!(todo_marker["severity"], "info");

        let unsafe_diag = diagnostics
            .iter()
            .find(|d| d["message"].as_str().expect("message should be a string").contains("unsafe"))
            .expect("should find unsafe");
        assert_eq!(unsafe_diag["severity"], "warning");

        for diag in &diagnostics {
            assert_eq!(diag["source"], "vscode-prehost-static-scan");
            assert!(diag["line"].as_u64().expect("line should be a number") > 0);
        }
    }

    #[test]
    fn diagnostics_reports_correct_line_numbers() {
        let content = "line one\nlet x = y.unwrap();\nline three\n// FIXME: broken\n";
        let diagnostics = collect_prehost_diagnostics(content);
        assert_eq!(diagnostics.len(), 2);
        let unwrap_diag = diagnostics
            .iter()
            .find(|d| d["message"].as_str().expect("message should be a string").contains("unwrap"))
            .expect("should find unwrap");
        assert_eq!(unwrap_diag["line"], 2);
        let fixme_diag = diagnostics
            .iter()
            .find(|d| d["message"].as_str().expect("message should be a string").contains("FIXME"))
            .expect("should find FIXME");
        assert_eq!(fixme_diag["line"], 4);
    }

    #[test]
    fn diagnostics_empty_for_clean_content() {
        let content = "fn clean() -> Result<(), Error> {\n    Ok(())\n}\n";
        let diagnostics = collect_prehost_diagnostics(content);
        assert!(diagnostics.is_empty(), "clean code should produce no diagnostics");
    }

    #[test]
    fn symbols_detects_extended_symbol_types() {
        let content = r#"
pub struct MyStruct {
    field: u32,
}

enum InternalEnum {
    A,
    B,
}

pub fn public_function() {}

fn private_function() {}

pub trait MyTrait {
    fn method(&self);
}

pub const MAX_SIZE: usize = 100;

pub static GLOBAL: u32 = 42;

static INTERNAL: u32 = 0;

pub type AliasType = Vec<String>;

type InternalAlias = HashMap<String, u32>;

pub use crate::other::Reexported;

macro_rules! my_macro {
    () => {};
}

impl MyStruct {
    fn new() -> Self { Self { field: 0 } }
}
"#;
        let symbols = collect_prehost_symbols(content);

        let kinds: Vec<&str> = symbols
            .iter()
            .map(|s| s["kind"].as_str().expect("kind should be a string"))
            .collect();
        assert!(kinds.contains(&"struct"), "should detect struct");
        assert!(kinds.contains(&"enum"), "should detect enum");
        assert!(kinds.contains(&"function"), "should detect function");
        assert!(kinds.contains(&"trait"), "should detect trait");
        assert!(kinds.contains(&"constant"), "should detect constant");
        assert!(kinds.contains(&"static"), "should detect static");
        assert!(kinds.contains(&"type_alias"), "should detect type_alias");
        assert!(kinds.contains(&"use_reexport"), "should detect use_reexport");
        assert!(kinds.contains(&"macro"), "should detect macro");
        assert!(kinds.contains(&"impl"), "should detect impl");

        let pub_struct = symbols
            .iter()
            .find(|s| s["name"] == "MyStruct" && s["kind"] == "struct")
            .expect("should find MyStruct");
        assert_eq!(pub_struct["visibility"], "public");

        let priv_enum = symbols
            .iter()
            .find(|s| s["name"] == "InternalEnum")
            .expect("should find InternalEnum");
        assert_eq!(priv_enum["visibility"], "private");

        let pub_static = symbols
            .iter()
            .find(|s| s["name"] == "GLOBAL")
            .expect("should find GLOBAL");
        assert_eq!(pub_static["kind"], "static");
        assert_eq!(pub_static["visibility"], "public");

        let priv_static = symbols
            .iter()
            .find(|s| s["name"] == "INTERNAL")
            .expect("should find INTERNAL static");
        assert_eq!(priv_static["visibility"], "private");

        let type_alias = symbols
            .iter()
            .find(|s| s["name"] == "AliasType")
            .expect("should find AliasType");
        assert_eq!(type_alias["kind"], "type_alias");
        assert_eq!(type_alias["visibility"], "public");

        let macro_sym = symbols
            .iter()
            .find(|s| s["name"] == "my_macro")
            .expect("should find my_macro");
        assert_eq!(macro_sym["kind"], "macro");

        for symbol in &symbols {
            assert_eq!(symbol["source"], "vscode-prehost-symbol-scan");
            assert!(symbol["line"].as_u64().expect("line should be a number") > 0);
            assert!(symbol["visibility"].as_str().is_some());
        }
    }

    #[test]
    fn symbols_preserves_line_numbers() {
        let content = "// comment\nfn first() {}\n// gap\npub struct Second {}\n";
        let symbols = collect_prehost_symbols(content);
        let first = symbols
            .iter()
            .find(|s| s["name"] == "first")
            .expect("should find first");
        assert_eq!(first["line"], 2);
        let second = symbols
            .iter()
            .find(|s| s["name"] == "Second")
            .expect("should find Second");
        assert_eq!(second["line"], 4);
    }

    #[test]
    fn symbols_empty_for_no_definitions() {
        let content = "// just a comment\nlet x = 42;\n";
        let symbols = collect_prehost_symbols(content);
        assert!(symbols.is_empty(), "content without definitions should produce no symbols");
    }
}
