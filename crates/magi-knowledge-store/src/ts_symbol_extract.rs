//! 基于 tree-sitter 的 AST 级符号提取。
//!
//! 替代 symbol_index 原本的逐行正则提取：对支持的语言用语法树精确抽取
//! 函数 / 类 / 接口 / 枚举 / 方法等定义，输出与正则路径一致的 SymbolEntry，
//! 不改变上层 search / rank 逻辑。不支持的语言由调用方回落到正则实现。

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

use crate::symbol_index::{SymbolEntry, SymbolKind};

/// 文件扩展名 → 是否由 tree-sitter 接管提取。
pub fn tree_sitter_supports(ext: &str) -> bool {
    language_for_ext(ext).is_some()
}

/// 用 tree-sitter 抽取一个文件的符号；语言不支持或解析失败返回 None，
/// 调用方据此回落到正则提取。
pub fn extract_symbols(file_path: &str, content: &str, ext: &str) -> Option<Vec<SymbolEntry>> {
    let lang = language_for_ext(ext)?;
    let spec = QuerySpec::for_ext(ext);

    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(content, None)?;

    let query = Query::new(&lang, spec.query_source).ok()?;
    let bytes = content.as_bytes();

    // 捕获组名 → 符号种类映射（@fn.name → Function 等）。
    let capture_names = query.capture_names();

    let mut cursor = QueryCursor::new();
    let mut out: Vec<SymbolEntry> = Vec::new();
    let mut matches = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let cap_name = capture_names[cap.index as usize];
            let Some(kind) = kind_for_capture(cap_name) else {
                continue;
            };
            let node = cap.node;
            let Ok(name) = node.utf8_text(bytes) else {
                continue;
            };
            let name = name.trim();
            if name.len() < 2 {
                continue;
            }
            // 定义节点的范围用父节点（声明整体），名字节点给出起始行。
            let def_node = node.parent().unwrap_or(node);
            let start_line = node.start_position().row;
            let end_line = def_node.end_position().row;
            // 方法记录所属类/接口名作为容器（向上找最近的类型声明节点）。
            let container = if kind == SymbolKind::Method {
                container_name(node, bytes)
            } else {
                None
            };
            out.push(SymbolEntry {
                name: name.to_string(),
                kind,
                file_path: file_path.to_string(),
                line: start_line,
                end_line: Some(end_line),
                is_exported: is_exported(content, start_line, ext),
                container,
                signature: None,
            });
        }
    }

    Some(out)
}

/// 简单导出判定：看定义所在行是否带 pub / export 前缀。
/// 仅用于排序加权的弱信号，不要求精确。
fn is_exported(content: &str, line: usize, ext: &str) -> bool {
    let Some(text) = content.lines().nth(line) else {
        return false;
    };
    let trimmed = text.trim_start();
    match ext {
        ".rs" => trimmed.starts_with("pub "),
        ".go" => text
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|s| !s.is_empty())
            .nth(1)
            .map(|ident| ident.chars().next().is_some_and(|c| c.is_uppercase()))
            .unwrap_or(false),
        _ => trimmed.starts_with("export "),
    }
}

/// 向上查找方法所属的类/接口名作为 container。
fn container_name(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        let kind = n.kind();
        if (kind == "class_declaration" || kind == "interface_declaration" || kind == "class")
            && let Some(name_node) = n.child_by_field_name("name")
            && let Ok(text) = name_node.utf8_text(bytes)
        {
            return Some(text.trim().to_string());
        }
        current = n.parent();
    }
    None
}

fn kind_for_capture(cap_name: &str) -> Option<SymbolKind> {
    match cap_name {
        "fn.name" => Some(SymbolKind::Function),
        "method.name" => Some(SymbolKind::Method),
        "class.name" => Some(SymbolKind::Class),
        "interface.name" => Some(SymbolKind::Interface),
        "type.name" => Some(SymbolKind::Type),
        "enum.name" => Some(SymbolKind::Enum),
        "var.name" => Some(SymbolKind::Variable),
        _ => None,
    }
}

fn language_for_ext(ext: &str) -> Option<tree_sitter::Language> {
    let lang = match ext {
        ".rs" => tree_sitter_rust::LANGUAGE.into(),
        ".ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ".tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        ".js" | ".jsx" | ".mjs" | ".cjs" => tree_sitter_javascript::LANGUAGE.into(),
        ".py" => tree_sitter_python::LANGUAGE.into(),
        ".go" => tree_sitter_go::LANGUAGE.into(),
        _ => return None,
    };
    Some(lang)
}

struct QuerySpec {
    query_source: &'static str,
}

impl QuerySpec {
    fn for_ext(ext: &str) -> Self {
        let query_source = match ext {
            ".rs" => RUST_QUERY,
            ".ts" | ".tsx" => TYPESCRIPT_QUERY,
            ".js" | ".jsx" | ".mjs" | ".cjs" => JAVASCRIPT_QUERY,
            ".py" => PYTHON_QUERY,
            ".go" => GO_QUERY,
            _ => "",
        };
        Self { query_source }
    }
}

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @fn.name)
(struct_item name: (type_identifier) @class.name)
(enum_item name: (type_identifier) @enum.name)
(trait_item name: (type_identifier) @interface.name)
(type_item name: (type_identifier) @type.name)
"#;

const TYPESCRIPT_QUERY: &str = r#"
(function_declaration name: (identifier) @fn.name)
(class_declaration name: (type_identifier) @class.name)
(interface_declaration name: (type_identifier) @interface.name)
(enum_declaration name: (identifier) @enum.name)
(type_alias_declaration name: (type_identifier) @type.name)
(method_definition name: (property_identifier) @method.name)
(lexical_declaration (variable_declarator name: (identifier) @var.name))
(variable_declaration (variable_declarator name: (identifier) @var.name))
(export_specifier alias: (identifier) @var.name)
(export_specifier name: (identifier) @var.name)
"#;

const JAVASCRIPT_QUERY: &str = r#"
(function_declaration name: (identifier) @fn.name)
(class_declaration name: (identifier) @class.name)
(method_definition name: (property_identifier) @method.name)
(lexical_declaration (variable_declarator name: (identifier) @var.name))
(variable_declaration (variable_declarator name: (identifier) @var.name))
(export_specifier alias: (identifier) @var.name)
(export_specifier name: (identifier) @var.name)
"#;

const PYTHON_QUERY: &str = r#"
(function_definition name: (identifier) @fn.name)
(class_definition name: (identifier) @class.name)
"#;

const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @fn.name)
(method_declaration name: (field_identifier) @method.name)
(type_declaration (type_spec name: (type_identifier) @type.name))
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols() {
        let src = "pub fn authenticate_user(t: &str) -> bool { !t.is_empty() }\n\
                   struct Session { id: u32 }\n\
                   enum Mode { A, B }\n";
        let syms = extract_symbols("src/auth.rs", src, ".rs").expect("rust supported");
        assert!(syms.iter().any(|s| s.name == "authenticate_user"
            && s.kind == SymbolKind::Function
            && s.is_exported));
        assert!(
            syms.iter()
                .any(|s| s.name == "Session" && s.kind == SymbolKind::Class)
        );
        assert!(
            syms.iter()
                .any(|s| s.name == "Mode" && s.kind == SymbolKind::Enum)
        );
    }

    #[test]
    fn extracts_typescript_symbols() {
        let src = "export function loadUser(id: string) { return id; }\n\
                   export class UserService { fetch() {} }\n\
                   interface UserDto { id: string }\n";
        let syms = extract_symbols("src/user.ts", src, ".ts").expect("ts supported");
        assert!(
            syms.iter()
                .any(|s| s.name == "loadUser" && s.kind == SymbolKind::Function)
        );
        assert!(
            syms.iter()
                .any(|s| s.name == "UserService" && s.kind == SymbolKind::Class)
        );
        assert!(
            syms.iter()
                .any(|s| s.name == "UserDto" && s.kind == SymbolKind::Interface)
        );
        assert!(
            syms.iter()
                .any(|s| s.name == "fetch" && s.kind == SymbolKind::Method)
        );
    }

    #[test]
    fn unsupported_language_returns_none() {
        assert!(extract_symbols("a.cpp", "int main() {}", ".cpp").is_none());
    }
}
