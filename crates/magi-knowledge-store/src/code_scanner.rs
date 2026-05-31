use crate::{CodeIndexIngestion, CodeIndexSource, KnowledgeStore};
use magi_core::UtcMillis;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 可索引的源代码扩展名集合（含点号，小写）
const INDEXED_EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".go", ".java", ".rs", ".c", ".h", ".cpp",
    ".cc", ".cxx", ".hpp", ".hh", ".cs", ".php", ".rb", ".swift", ".kt", ".kts", ".m", ".mm",
    ".vue", ".svelte", ".json", ".md", ".toml", ".yml", ".yaml",
];

/// 默认忽略模式（目录和文件）
const IGNORE_PATTERNS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "out",
    "build",
    ".next",
    ".nuxt",
    ".output",
    "__pycache__",
    ".vscode",
    ".idea",
    ".history",
    "coverage",
    ".nyc_output",
    ".magi",
    ".claude",
];

/// 入口文件检测模式
const ENTRY_PATTERNS: &[&str] = &[
    "index.ts",
    "index.js",
    "main.ts",
    "main.js",
    "app.ts",
    "app.js",
    "lib.rs",
    "main.rs",
    "src/index.ts",
    "src/index.js",
    "src/main.ts",
    "src/main.js",
    "src/lib.rs",
    "src/main.rs",
];

/// 技术栈检测映射：(配置文件, 技术栈名称)
const TECH_STACK_DETECTORS: &[(&str, &str)] = &[
    ("tsconfig.json", "TypeScript"),
    ("package.json", "JavaScript"),
    ("Cargo.toml", "Rust"),
    ("go.mod", "Go"),
    ("pyproject.toml", "Python"),
    ("pom.xml", "Java"),
    ("build.gradle", "Java/Kotlin"),
];

/// package.json 依赖到框架/工具的映射
const FRAMEWORK_DEPS: &[(&str, &str)] = &[
    ("react", "React"),
    ("vue", "Vue"),
    ("svelte", "Svelte"),
    ("next", "Next.js"),
    ("nuxt", "Nuxt"),
    ("express", "Express"),
    ("fastify", "Fastify"),
    ("nestjs", "NestJS"),
    ("@nestjs/core", "NestJS"),
    ("electron", "Electron"),
];

const BUILD_TOOL_DEPS: &[(&str, &str)] = &[
    ("webpack", "Webpack"),
    ("vite", "Vite"),
    ("rollup", "Rollup"),
    ("esbuild", "esbuild"),
    ("turbo", "Turborepo"),
    ("tsup", "tsup"),
    ("parcel", "Parcel"),
    ("swc", "SWC"),
    ("@swc/core", "SWC"),
];

const TEST_DEPS: &[(&str, &str)] = &[
    ("jest", "Jest"),
    ("mocha", "Mocha"),
    ("vitest", "Vitest"),
    ("@playwright/test", "Playwright"),
    ("cypress", "Cypress"),
    ("ava", "AVA"),
];

/// 单个扫描到的文件信息
#[derive(Clone, Debug)]
pub struct ScannedFile {
    pub path: String,
    pub language: Option<String>,
    pub size: u64,
    pub lines: usize,
}

/// 代码扫描结果摘要
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CodeIndexSummary {
    pub files: Vec<CodeIndexFile>,
    pub tech_stack: Vec<String>,
    pub entry_points: Vec<String>,
    pub last_indexed: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CodeIndexFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexScanStatus {
    Indexed,
    Empty,
    Failed,
}

impl CodeIndexScanStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::Empty => "empty",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexScanReasonCode {
    WorkspaceMissing,
    WorkspaceNotDirectory,
    WorkspaceUnreadable,
    NoIndexableFiles,
}

impl CodeIndexScanReasonCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WorkspaceMissing => "workspace_missing",
            Self::WorkspaceNotDirectory => "workspace_not_directory",
            Self::WorkspaceUnreadable => "workspace_unreadable",
            Self::NoIndexableFiles => "no_indexable_files",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CodeIndexScanOutcome {
    pub status: CodeIndexScanStatus,
    pub reason_code: Option<CodeIndexScanReasonCode>,
    pub summary: Option<CodeIndexSummary>,
}

impl CodeIndexScanOutcome {
    fn indexed(summary: CodeIndexSummary) -> Self {
        Self {
            status: CodeIndexScanStatus::Indexed,
            reason_code: None,
            summary: Some(summary),
        }
    }

    pub fn indexed_existing() -> Self {
        Self {
            status: CodeIndexScanStatus::Indexed,
            reason_code: None,
            summary: None,
        }
    }

    fn empty(reason_code: CodeIndexScanReasonCode) -> Self {
        Self {
            status: CodeIndexScanStatus::Empty,
            reason_code: Some(reason_code),
            summary: None,
        }
    }

    fn failed(reason_code: CodeIndexScanReasonCode) -> Self {
        Self {
            status: CodeIndexScanStatus::Failed,
            reason_code: Some(reason_code),
            summary: None,
        }
    }
}

/// 扫描工作区代码并生成代码索引
pub fn scan_workspace(workspace_root: &Path) -> CodeIndexScanOutcome {
    if !workspace_root.exists() {
        return CodeIndexScanOutcome::failed(CodeIndexScanReasonCode::WorkspaceMissing);
    }
    if !workspace_root.is_dir() {
        return CodeIndexScanOutcome::failed(CodeIndexScanReasonCode::WorkspaceNotDirectory);
    }
    if std::fs::read_dir(workspace_root).is_err() {
        return CodeIndexScanOutcome::failed(CodeIndexScanReasonCode::WorkspaceUnreadable);
    }

    let mut files: Vec<ScannedFile> = Vec::new();
    let mut tech_stack = Vec::new();
    let mut entry_points = Vec::new();

    scan_directory(workspace_root, workspace_root, &mut files);
    detect_tech_stack(workspace_root, &files, &mut tech_stack);
    detect_entry_points(&files, &mut entry_points);

    let index_files: Vec<CodeIndexFile> = files
        .into_iter()
        .map(|f| CodeIndexFile {
            path: f.path,
            lines: Some(f.lines),
            size: Some(f.size),
        })
        .collect();

    if index_files.is_empty() {
        return CodeIndexScanOutcome::empty(CodeIndexScanReasonCode::NoIndexableFiles);
    }

    CodeIndexScanOutcome::indexed(CodeIndexSummary {
        files: index_files,
        tech_stack,
        entry_points,
        last_indexed: UtcMillis::now().0,
    })
}

/// 将扫描结果摄取到知识存储中
pub fn ingest_workspace_code_index(
    store: &KnowledgeStore,
    workspace_root: &Path,
) -> CodeIndexScanOutcome {
    let outcome = scan_workspace(workspace_root);
    let Some(summary) = outcome.summary.as_ref() else {
        store.delete_project_code_index();
        return outcome;
    };
    if let Some(ingestion) = code_index_ingestion_for_summary(workspace_root, summary) {
        store.ingest_code_index(ingestion);
    }
    outcome
}

pub(crate) fn code_index_ingestion_for_summary(
    workspace_root: &Path,
    summary: &CodeIndexSummary,
) -> Option<CodeIndexIngestion> {
    let content = match serde_json::to_string(&summary) {
        Ok(json) => json,
        Err(_) => return None,
    };

    Some(CodeIndexIngestion {
        knowledge_id: "project-code-index".to_string(),
        title: format!("Project Code Index: {}", workspace_root.display()),
        content,
        tags: summary.tech_stack.clone(),
        source_ref: Some(workspace_root.to_string_lossy().to_string()),
        updated_at: UtcMillis::now(),
        source: CodeIndexSource {
            path: workspace_root.to_string_lossy().to_string(),
            language: None,
            repo_ref: None,
            commit_ref: None,
            start_line: Some(summary.files.len()),
            end_line: None,
            symbol: None,
        },
        audit: None,
        governance: None,
    })
}

fn scan_directory(root: &Path, current: &Path, files: &mut Vec<ScannedFile>) {
    let entries = match std::fs::read_dir(current) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if should_ignore(&name) {
            continue;
        }

        if path.is_dir() {
            scan_directory(root, &path, files);
        } else if path.is_file() {
            if let Some(relative) = pathdiff::diff_paths(&path, root) {
                let rel_str = relative.to_string_lossy().replace('\\', "/");
                if should_index(&rel_str) {
                    if let Ok(metadata) = std::fs::metadata(&path) {
                        let size = metadata.len();
                        let lines = count_lines(&path);
                        let language = detect_language(&rel_str);
                        files.push(ScannedFile {
                            path: rel_str,
                            language,
                            size,
                            lines,
                        });
                    }
                }
            }
        }
    }
}

fn should_ignore(name: &str) -> bool {
    IGNORE_PATTERNS.iter().any(|pattern| name == *pattern)
}

fn should_index(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext.is_empty() {
        return false;
    }
    let dotted = format!(".{ext}");
    INDEXED_EXTENSIONS.contains(&dotted.as_str())
}

fn count_lines(path: &Path) -> usize {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let mut buf = [0u8; 8192];
    let mut count = 0;
    let mut total = 0usize;

    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for &byte in &buf[..n] {
                    if byte == b'\n' {
                        count += 1;
                    }
                }
                total += n;
            }
            Err(_) => break,
        }
    }

    // 文件非空且末尾无换行 → 最后一行也计入
    if total > 0 {
        let mut last_byte = [0u8; 1];
        if std::fs::File::open(path)
            .and_then(|mut f| {
                f.seek(SeekFrom::End(-1))?;
                f.read_exact(&mut last_byte)?;
                Ok(())
            })
            .is_ok()
            && last_byte[0] != b'\n'
        {
            count += 1;
        }
    }

    count
}

fn detect_language(path: &str) -> Option<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?;
    match ext.to_lowercase().as_str() {
        "ts" | "tsx" => Some("TypeScript".to_string()),
        "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript".to_string()),
        "rs" => Some("Rust".to_string()),
        "py" => Some("Python".to_string()),
        "go" => Some("Go".to_string()),
        "java" => Some("Java".to_string()),
        "json" => Some("JSON".to_string()),
        "md" => Some("Markdown".to_string()),
        "yml" | "yaml" => Some("YAML".to_string()),
        "vue" => Some("Vue".to_string()),
        "svelte" => Some("Svelte".to_string()),
        _ => None,
    }
}

fn detect_tech_stack(root: &Path, files: &[ScannedFile], tech_stack: &mut Vec<String>) {
    let mut seen = HashSet::new();
    let mut package_json_paths = Vec::new();

    for file in files {
        for &(config_file, name) in TECH_STACK_DETECTORS {
            if file.path == config_file || file.path.ends_with(&format!("/{config_file}")) {
                seen.insert(name.to_string());
            }
        }
        if file.path == "package.json" || file.path.ends_with("/package.json") {
            package_json_paths.push(root.join(&file.path));
        }
    }

    // 从所有被索引的 package.json 中读取框架和工具，覆盖 monorepo / 多子项目结构
    for package_json in package_json_paths {
        if let Ok(content) = std::fs::read_to_string(&package_json) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                let deps = value.get("dependencies").and_then(|d| d.as_object());
                let dev_deps = value.get("devDependencies").and_then(|d| d.as_object());

                let all_deps: HashSet<String> = deps
                    .into_iter()
                    .chain(dev_deps.into_iter())
                    .flat_map(|m| m.keys().cloned())
                    .collect();

                for &(dep, name) in FRAMEWORK_DEPS {
                    if all_deps.contains(dep) {
                        seen.insert(name.to_string());
                    }
                }
                for &(dep, name) in BUILD_TOOL_DEPS {
                    if all_deps.contains(dep) {
                        seen.insert(name.to_string());
                    }
                }
                for &(dep, name) in TEST_DEPS {
                    if all_deps.contains(dep) {
                        seen.insert(name.to_string());
                    }
                }

                // npm scripts 中有 build → 标记 npm scripts
                if value.get("scripts").and_then(|s| s.get("build")).is_some() {
                    seen.insert("npm scripts".to_string());
                }
            }
        }
    }

    *tech_stack = seen.into_iter().collect();
    tech_stack.sort();
}

fn detect_entry_points(files: &[ScannedFile], entry_points: &mut Vec<String>) {
    let mut seen = HashSet::new();
    for file in files {
        for pattern in ENTRY_PATTERNS {
            if file.path.ends_with(pattern) && !seen.contains(&file.path) {
                seen.insert(file.path.clone());
                break;
            }
        }
    }
    *entry_points = seen.into_iter().collect();
    entry_points.sort();
}

// 需要 pathdiff crate 或手动实现相对路径计算
// 为了避免增加依赖，我们手动实现一个简单的相对路径计算
mod pathdiff {
    use std::path::{Component, Path, PathBuf};

    pub fn diff_paths(path: &Path, base: &Path) -> Option<PathBuf> {
        let path = path.components().collect::<Vec<_>>();
        let base = base.components().collect::<Vec<_>>();

        let mut i = 0;
        while i < path.len() && i < base.len() && path[i] == base[i] {
            i += 1;
        }

        let mut result = PathBuf::new();
        for _ in i..base.len() {
            result.push(Component::ParentDir);
        }
        for component in &path[i..] {
            result.push(component);
        }

        if result.as_os_str().is_empty() {
            Some(PathBuf::from("."))
        } else {
            Some(result)
        }
    }
}
