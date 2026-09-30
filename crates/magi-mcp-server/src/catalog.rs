//! 对外工具目录：公开名（命名空间化）与 Magi 内部工具的映射、按权限档生成的目录。
//!
//! 本 crate 不依赖工具运行时：工具说明与参数 schema 由宿主通过 [`ToolSchemaProvider`]
//! 提供（宿主直接从 `BuiltinToolName` 取），这样 schema 只有一个事实源，不在这里复制。
//!
//! 第一批只包含**已在工具运行时核对过存在**的工具；`magi.git.*`、`magi.changes.*`
//! 等在核对现有能力后再加入，见《Magi MCP 服务设计》§6。

use serde_json::{Value, json};

use crate::profile::{Profile, ToolClass};

/// 公开名与内部工具名的映射。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolMapping {
    /// 对外公开名（命名空间化）。
    pub public_name: &'static str,
    /// Magi 工具运行时里的内部名。
    pub internal_name: &'static str,
    pub class: ToolClass,
}

/// 第一批对外工具。顺序即目录顺序。
pub const V1_TOOLS: &[ToolMapping] = &[
    ToolMapping {
        public_name: "magi.fs.read",
        internal_name: "file_read",
        class: ToolClass::Read,
    },
    ToolMapping {
        public_name: "magi.search.text",
        internal_name: "search_text",
        class: ToolClass::Read,
    },
    ToolMapping {
        public_name: "magi.search.semantic",
        internal_name: "search_semantic",
        class: ToolClass::Read,
    },
    ToolMapping {
        public_name: "magi.fs.write",
        internal_name: "file_write",
        class: ToolClass::Write,
    },
    ToolMapping {
        public_name: "magi.fs.patch",
        internal_name: "file_patch",
        class: ToolClass::Write,
    },
    ToolMapping {
        public_name: "magi.fs.apply_patch",
        internal_name: "apply_patch",
        class: ToolClass::Write,
    },
    ToolMapping {
        public_name: "magi.fs.mkdir",
        internal_name: "file_mkdir",
        class: ToolClass::Write,
    },
    ToolMapping {
        public_name: "magi.fs.move",
        internal_name: "file_move",
        class: ToolClass::Write,
    },
    ToolMapping {
        public_name: "magi.fs.copy",
        internal_name: "file_copy",
        class: ToolClass::Write,
    },
    ToolMapping {
        public_name: "magi.fs.remove",
        internal_name: "file_remove",
        class: ToolClass::Destructive,
    },
    ToolMapping {
        public_name: "magi.shell.exec",
        internal_name: "shell_exec",
        class: ToolClass::Exec,
    },
];

pub fn mapping_for_public_name(public_name: &str) -> Option<&'static ToolMapping> {
    V1_TOOLS
        .iter()
        .find(|mapping| mapping.public_name == public_name)
}

/// 宿主提供的单个工具的说明与参数 schema。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSchema {
    pub description: String,
    pub input_schema: Value,
}

pub trait ToolSchemaProvider: Send + Sync {
    /// 按**内部名**取说明与 schema；宿主不认识该工具时返回 `None`（该工具不进入目录）。
    fn schema_for(&self, internal_name: &str) -> Option<ToolSchema>;
}

/// 目录里的一个工具。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub class: ToolClass,
}

impl ToolDescriptor {
    /// MCP `tools/list` 里的一项，带只读 / 破坏性等注解，客户端可据此决定是否再次确认。
    pub fn to_mcp_value(&self) -> Value {
        let read_only = self.class == ToolClass::Read;
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": {
                "readOnlyHint": read_only,
                "destructiveHint": matches!(self.class, ToolClass::Destructive | ToolClass::Exec),
                "idempotentHint": read_only,
                // 全部是本机工作区操作，不与开放世界交互。
                "openWorldHint": false
            }
        })
    }
}

/// 按权限档生成目录：只包含该档能调用、且宿主提供了 schema 的工具。
pub fn build_catalog(profile: Profile, provider: &dyn ToolSchemaProvider) -> Vec<ToolDescriptor> {
    V1_TOOLS
        .iter()
        .filter(|mapping| profile.allows(mapping.class))
        .filter_map(|mapping| {
            let schema = provider.schema_for(mapping.internal_name)?;
            Some(ToolDescriptor {
                name: mapping.public_name.to_string(),
                description: schema.description,
                input_schema: schema.input_schema,
                class: mapping.class,
            })
        })
        .collect()
}

/// `tools/list` 的返回。
pub fn tools_list_result(catalog: &[ToolDescriptor]) -> Value {
    json!({ "tools": catalog.iter().map(ToolDescriptor::to_mcp_value).collect::<Vec<_>>() })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AllTools;
    impl ToolSchemaProvider for AllTools {
        fn schema_for(&self, internal_name: &str) -> Option<ToolSchema> {
            Some(ToolSchema {
                description: format!("{internal_name} 说明"),
                input_schema: json!({ "type": "object" }),
            })
        }
    }

    struct OnlyRead;
    impl ToolSchemaProvider for OnlyRead {
        fn schema_for(&self, internal_name: &str) -> Option<ToolSchema> {
            (internal_name == "file_read").then(|| ToolSchema {
                description: "读取".to_string(),
                input_schema: json!({ "type": "object" }),
            })
        }
    }

    fn names(catalog: &[ToolDescriptor]) -> Vec<&str> {
        catalog.iter().map(|tool| tool.name.as_str()).collect()
    }

    #[test]
    fn catalog_only_contains_what_the_profile_may_call() {
        let read_only = build_catalog(Profile::ReadOnly, &AllTools);
        assert_eq!(
            names(&read_only),
            ["magi.fs.read", "magi.search.text", "magi.search.semantic"]
        );

        let edit = build_catalog(Profile::Edit, &AllTools);
        assert!(names(&edit).contains(&"magi.fs.write"));
        assert!(names(&edit).contains(&"magi.fs.remove"));
        assert!(!names(&edit).contains(&"magi.shell.exec"));

        let exec = build_catalog(Profile::Exec, &AllTools);
        assert!(names(&exec).contains(&"magi.shell.exec"));
    }

    #[test]
    fn tools_the_host_does_not_know_never_enter_the_catalog() {
        assert_eq!(
            names(&build_catalog(Profile::Exec, &OnlyRead)),
            ["magi.fs.read"]
        );
    }

    #[test]
    fn public_names_are_unique_namespaced_and_resolve_back() {
        let mut seen = std::collections::HashSet::new();
        for mapping in V1_TOOLS {
            assert!(
                mapping.public_name.starts_with("magi."),
                "{}",
                mapping.public_name
            );
            assert!(seen.insert(mapping.public_name));
            assert_eq!(
                mapping_for_public_name(mapping.public_name).map(|m| m.internal_name),
                Some(mapping.internal_name)
            );
        }
        // 内部名不能直接当公开名调用。
        assert!(mapping_for_public_name("file_read").is_none());
    }

    #[test]
    fn annotations_mark_read_only_and_destructive_tools() {
        let catalog = build_catalog(Profile::Exec, &AllTools);
        let by_name = |name: &str| {
            catalog
                .iter()
                .find(|tool| tool.name == name)
                .unwrap()
                .to_mcp_value()
        };
        assert_eq!(by_name("magi.fs.read")["annotations"]["readOnlyHint"], true);
        assert_eq!(
            by_name("magi.fs.write")["annotations"]["readOnlyHint"],
            false
        );
        assert_eq!(
            by_name("magi.fs.write")["annotations"]["destructiveHint"],
            false
        );
        assert_eq!(
            by_name("magi.fs.remove")["annotations"]["destructiveHint"],
            true
        );
        assert_eq!(
            by_name("magi.shell.exec")["annotations"]["destructiveHint"],
            true
        );
        assert_eq!(
            by_name("magi.fs.read")["annotations"]["openWorldHint"],
            false
        );
    }
}
