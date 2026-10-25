import { getFileExtension } from './file-preview-utils';
import type { IconName } from './icons';

/** Visual identity only: this catalog does not grant preview, execution, or upload support. */
export interface FileTypeDefinition {
  kind: string;
  category: string;
  label: string;
  icon: IconName;
  extensions: readonly string[];
  filenames?: readonly string[];
  example: string;
}

// Keep registration and preview together so newly supported identities stay discoverable.
export const fileTypeCatalog = [
  { kind: "svelte", category: "code", label: "Svelte", icon: "file-code",
    extensions: ["svelte"], example: "Component.svelte" },
  { kind: "vue", category: "code", label: "Vue", icon: "file-code",
    extensions: ["vue"], example: "Component.vue" },
  { kind: "typescript", category: "code", label: "TypeScript", icon: "file-code",
    extensions: ["ts", "mts", "cts", "d.ts", "d.mts", "d.cts"], example: "runtime.ts" },
  { kind: "javascript", category: "code", label: "JavaScript", icon: "file-code",
    extensions: ["js", "mjs", "cjs"], example: "script.mjs" },
  { kind: "react", category: "code", label: "React", icon: "file-code",
    extensions: ["tsx", "jsx"], example: "Component.tsx" },
  { kind: "rust", category: "code", label: "Rust", icon: "file-code",
    extensions: ["rs"], filenames: ["Cargo.toml", "Cargo.lock"], example: "main.rs" },
  { kind: "python", category: "code", label: "Python", icon: "file-code",
    extensions: ["py", "pyw", "pyi", "pyx", "pxd", "pxi", "ipynb"], example: "analysis.ipynb" },
  { kind: "go", category: "code", label: "Go", icon: "file-code",
    extensions: ["go"], filenames: ["go.mod", "go.sum", "go.work"], example: "main.go" },
  { kind: "java", category: "code", label: "Java", icon: "file-code",
    extensions: ["java", "jsp", "jspx"], example: "Main.java" },
  { kind: "kotlin", category: "code", label: "Kotlin", icon: "file-code",
    extensions: ["kt", "kts"], example: "Main.kt" },
  { kind: "c", category: "code", label: "C", icon: "file-code",
    extensions: ["c", "h"], example: "main.c" },
  { kind: "cpp", category: "code", label: "C++", icon: "file-code",
    extensions: ["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ipp", "tpp", "ixx", "cppm"], example: "main.cpp" },
  { kind: "csharp", category: "code", label: "C# / .NET", icon: "file-code",
    extensions: ["cs", "csx", "csproj", "fsproj", "vbproj", "sln", "slnx", "razor", "cshtml"], example: "App.cs" },
  { kind: "swift", category: "code", label: "Swift / Objective-C", icon: "file-code",
    extensions: ["swift", "m", "mm"], example: "App.swift" },
  { kind: "dart", category: "code", label: "Dart", icon: "file-code",
    extensions: ["dart"], example: "main.dart" },
  { kind: "ruby", category: "code", label: "Ruby", icon: "file-code",
    extensions: ["rb", "rbw", "rake", "gemspec", "erb"], filenames: ["Gemfile", "Gemfile.lock", "Rakefile"], example: "app.rb" },
  { kind: "php", category: "code", label: "PHP", icon: "file-code",
    extensions: ["php", "php3", "php4", "php5", "php7", "php8", "phtml", "phps", "blade.php"], example: "index.php" },
  { kind: "lua", category: "code", label: "Lua", icon: "file-code",
    extensions: ["lua", "luau"], example: "init.lua" },
  { kind: "r", category: "code", label: "R", icon: "file-code",
    extensions: ["r", "rmd", "rproj"], example: "analysis.R" },
  { kind: "functional", category: "code", label: "Functional language", icon: "file-code",
    extensions: ["scala", "sc", "ex", "exs", "erl", "hrl", "hs", "lhs", "clj", "cljs", "cljc", "edn", "fs", "fsx", "fsi", "ml", "mli", "elm"], example: "app.ex" },
  { kind: "code", category: "code", label: "Source code", icon: "file-code",
    extensions: ["pl", "pm", "raku", "jl", "nim", "zig", "v", "d", "pas", "pp", "f", "f90", "f95", "for", "groovy", "gradle", "asm", "s", "vb", "vbs", "sol", "move"], example: "main.zig" },
  { kind: "html", category: "web", label: "HTML / XML", icon: "file-code",
    extensions: ["html", "htm", "xhtml", "xml", "xsl", "xslt", "xsd", "dtd", "xul", "svgx", "astro", "hbs", "handlebars", "mustache", "ejs", "pug", "jade", "liquid", "twig"], example: "index.html" },
  { kind: "css", category: "web", label: "Stylesheet", icon: "file-code",
    extensions: ["css", "scss", "sass", "less", "styl", "stylus", "postcss", "pcss"], example: "styles.css" },
  { kind: "markdown", category: "data", label: "Markdown", icon: "file-text",
    extensions: ["md", "markdown", "mdx", "mdown", "mkd"], filenames: ["README"], example: "README.md" },
  { kind: "text", category: "data", label: "Plain text", icon: "file-text",
    extensions: ["txt", "text", "log", "rst", "adoc", "asciidoc", "nfo", "out", "err", "org", "tex", "latex", "bib"], filenames: ["LICENSE", "LICENCE", "NOTICE", "AUTHORS", "CONTRIBUTORS", "CHANGELOG", "CHANGES", "COPYING", "TODO"], example: "notes.txt" },
  { kind: "json", category: "data", label: "JSON", icon: "file-json",
    extensions: ["json", "jsonc", "json5", "jsonl", "ndjson", "geojson", "topojson", "webmanifest", "map", "har"], example: "package.json" },
  { kind: "yaml", category: "config", label: "YAML", icon: "file-cog",
    extensions: ["yaml", "yml"], example: "config.yaml" },
  { kind: "toml", category: "config", label: "TOML", icon: "file-cog",
    extensions: ["toml"], example: "pyproject.toml" },
  { kind: "config", category: "config", label: "Configuration", icon: "file-cog",
    extensions: ["ini", "conf", "cfg", "config", "properties", "prefs", "settings", "cnf", "editorconfig", "npmrc", "yarnrc", "bowerrc", "browserslistrc"], filenames: [".editorconfig", ".npmrc", ".yarnrc", ".nvmrc", ".node-version", ".python-version", ".browserslistrc", ".eslintrc", ".prettierrc", ".stylelintrc", ".babelrc", ".watchmanconfig", ".tool-versions"], example: "app.conf" },
  { kind: "env", category: "config", label: "Environment", icon: "file-lock",
    extensions: ["env"], filenames: [".env"], example: "local.env" },
  { kind: "docker", category: "config", label: "Container", icon: "file-cog",
    extensions: ["dockerfile", "containerfile"], filenames: ["Dockerfile", "Containerfile", ".dockerignore"], example: "Dockerfile" },
  { kind: "git", category: "config", label: "Git", icon: "git-branch",
    extensions: ["diff", "patch"], filenames: [".gitignore", ".gitattributes", ".gitmodules", ".gitconfig", ".gitkeep", ".gitmessage"], example: "changes.patch" },
  { kind: "build", category: "config", label: "Build / infrastructure", icon: "file-cog",
    extensions: ["cmake", "mk", "make", "ninja", "bazel", "bzl", "tf", "tfvars", "hcl", "nix", "qml", "pro", "pri", "vcxproj", "props", "targets"], filenames: ["Makefile", "GNUmakefile", "CMakeLists.txt", "BUILD", "BUILD.bazel", "WORKSPACE", "WORKSPACE.bazel", "meson.build", "Jenkinsfile", "Vagrantfile", "Procfile", "Justfile", "Taskfile"], example: "main.tf" },
  { kind: "lock", category: "config", label: "Dependency lockfile", icon: "file-lock",
    extensions: ["lock"], filenames: ["yarn.lock", "bun.lock", "bun.lockb", "poetry.lock", "uv.lock", "composer.lock"], example: "yarn.lock" },
  { kind: "shell", category: "config", label: "Shell / command script", icon: "terminal",
    extensions: ["sh", "bash", "zsh", "fish", "ksh", "csh", "tcsh", "ps1", "psm1", "psd1", "bat", "cmd", "command"], filenames: [".bashrc", ".bash_profile", ".zshrc", ".zprofile", ".profile"], example: "setup.ps1" },
  { kind: "sql", category: "data", label: "SQL", icon: "database",
    extensions: ["sql", "ddl", "dml"], example: "query.sql" },
  { kind: "database", category: "data", label: "Database", icon: "database",
    extensions: ["db", "sqlite", "sqlite3", "db3", "mdb", "accdb", "rdb", "dbf", "duckdb", "realm"], example: "data.sqlite" },
  { kind: "data", category: "data", label: "Structured / scientific data", icon: "file-json",
    extensions: ["parquet", "arrow", "feather", "avro", "orc", "h5", "hdf5", "hdf", "nc", "mat", "npy", "npz", "pkl", "pickle", "joblib", "sav", "dta", "sas7bdat", "bson", "msgpack", "graphql", "gql", "proto", "thrift", "capnp"], example: "dataset.parquet" },
  { kind: "pdf", category: "office", label: "PDF", icon: "file-text",
    extensions: ["pdf"], example: "report.pdf" },
  { kind: "document", category: "office", label: "Word / text document", icon: "file-text",
    extensions: ["doc", "docx", "docm", "dot", "dotx", "dotm", "odt", "ott", "rtf", "pages", "wps", "wpt"], example: "proposal.docx" },
  { kind: "spreadsheet", category: "office", label: "Spreadsheet", icon: "file-text",
    extensions: ["xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "xltm", "ods", "ots", "csv", "tsv", "numbers", "et", "ett"], example: "budget.xlsx" },
  { kind: "presentation", category: "office", label: "Presentation", icon: "file-text",
    extensions: ["ppt", "pptx", "pptm", "pot", "potx", "potm", "pps", "ppsx", "ppsm", "odp", "otp", "dps", "dpt"], example: "slides.pptx" },
  { kind: "ebook", category: "office", label: "E-book", icon: "file-text",
    extensions: ["epub", "mobi", "azw", "azw3", "fb2", "djvu", "djv", "cbz", "cbr"], example: "book.epub" },
  { kind: "email", category: "office", label: "Email / calendar / contact", icon: "file-text",
    extensions: ["eml", "msg", "mbox", "pst", "ost", "ics", "ical", "vcf", "vcard"], example: "meeting.ics" },
  { kind: "image", category: "image", label: "Image", icon: "image",
    extensions: ["png", "jpg", "jpeg", "jpe", "jfif", "gif", "webp", "avif", "bmp", "dib", "tif", "tiff", "ico", "icns", "heic", "heif", "jxl", "apng", "pnm", "ppm", "pgm", "pbm", "tga", "exr", "hdr", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2"], example: "photo.heic" },
  { kind: "vector", category: "image", label: "Vector image", icon: "image",
    extensions: ["svg", "svgz", "eps", "ps"], example: "logo.svg" },
  { kind: "design", category: "image", label: "Design source", icon: "image",
    extensions: ["psd", "psb", "ai", "aep", "aepx", "prproj", "sketch", "fig", "xd", "afdesign", "afphoto", "afpub", "indd", "idml", "xcf", "kra", "procreate"], example: "design.fig" },
  { kind: "audio", category: "media", label: "Audio", icon: "music",
    extensions: ["mp3", "wav", "flac", "ogg", "oga", "opus", "aac", "m4a", "aiff", "aif", "alac", "wma", "amr", "mid", "midi", "caf", "ac3", "aax", "au"], example: "audio.flac" },
  { kind: "video", category: "media", label: "Video", icon: "film",
    extensions: ["mp4", "m4v", "mov", "avi", "mkv", "webm", "wmv", "flv", "mpg", "mpeg", "m2v", "m2ts", "m3u8", "3gp", "3g2", "vob", "ogv", "rm", "rmvb", "vtt", "srt", "ass", "ssa", "sub", "sbv"], example: "clip.mp4" },
  { kind: "binary", category: "archive", label: "Archive", icon: "file-archive",
    extensions: ["zip", "zipx", "rar", "7z", "tar", "gz", "gzip", "tgz", "bz", "bz2", "tbz", "tbz2", "xz", "txz", "z", "zst", "zstd", "tzst", "lz", "lzma", "lz4", "br", "cab", "ar", "cpio", "tar.gz", "tar.bz2", "tar.xz", "tar.zst"], example: "backup.tar.gz" },
  { kind: "disk", category: "archive", label: "Disk image", icon: "hard-drive",
    extensions: ["iso", "img", "dmg", "vhd", "vhdx", "vmdk", "vdi", "qcow", "qcow2", "wim"], example: "disk.dmg" },
  { kind: "font", category: "system", label: "Font", icon: "type",
    extensions: ["ttf", "otf", "woff", "woff2", "eot", "ttc", "otc", "pfb", "pfm", "bdf"], example: "font.woff2" },
  { kind: "executable", category: "system", label: "Application / installer", icon: "file-cog",
    extensions: ["exe", "msi", "msix", "msixbundle", "appx", "appxbundle", "appimage", "deb", "rpm", "apk", "aab", "ipa", "pkg", "app", "run", "com", "scr"], example: "installer.exe" },
  { kind: "binary-code", category: "system", label: "Binary / library", icon: "file-code",
    extensions: ["bin", "dat", "dll", "so", "dylib", "a", "lib", "o", "class", "jar", "war", "ear", "aar", "wasm", "pdb", "pyc", "pyo", "whl", "egg", "gem"], example: "module.wasm" },
  { kind: "certificate", category: "system", label: "Certificate / key", icon: "file-lock",
    extensions: ["pem", "key", "crt", "cer", "der", "p12", "pfx", "p7b", "p7c", "csr", "pub", "asc", "gpg", "kdbx", "keystore", "jks"], example: "certificate.pem" },
  { kind: "model", category: "engineering", label: "3D / CAD", icon: "box",
    extensions: ["stl", "obj", "fbx", "blend", "gltf", "glb", "usd", "usdz", "usda", "usdc", "dae", "3ds", "max", "ma", "mb", "c4d", "step", "stp", "iges", "igs", "dwg", "dxf", "3mf", "scad", "skp"], example: "model.glb" },
  { kind: "ai-model", category: "engineering", label: "AI model", icon: "brain",
    extensions: ["safetensors", "gguf", "ggml", "onnx", "pt", "pth", "ckpt", "tflite", "pb", "mlmodel", "coreml"], example: "model.gguf" },
  { kind: "diagram", category: "engineering", label: "Diagram / mind map", icon: "layers",
    extensions: ["drawio", "excalidraw", "mmd", "mermaid", "puml", "plantuml", "gv", "mindnode", "xmind", "vsd", "vsdx"], example: "diagram.drawio" },
  { kind: "notebook", category: "engineering", label: "Notebook / research", icon: "file-text",
    extensions: ["nb", "qmd", "rnw", "typ"], example: "research.qmd" },
] as const satisfies readonly FileTypeDefinition[];

export type FileTypeVisualKind = typeof fileTypeCatalog[number]['kind'] | 'generic';
export interface FileTypeVisual {
  kind: FileTypeVisualKind;
  icon: IconName;
  label: string;
}

const extensionVisuals = new Map<string, FileTypeVisual>();
const fileNameVisuals = new Map<string, FileTypeVisual>();
for (const definition of fileTypeCatalog) {
  const visual: FileTypeVisual = { kind: definition.kind, icon: definition.icon, label: definition.label };
  for (const extension of definition.extensions) extensionVisuals.set(extension, visual);
  for (const name of ('filenames' in definition ? definition.filenames : [])) fileNameVisuals.set(name.toLowerCase(), visual);
}
const compoundExtensions = [...extensionVisuals.keys()].filter(extension => extension.includes('.'))
  .sort((left, right) => right.length - left.length);
const genericVisual: FileTypeVisual = { kind: 'generic', icon: 'file', label: 'File' };

export function getFileTypeVisual(path: string): FileTypeVisual {
  const name = path.split(/[\\/]/u).pop()?.toLowerCase() ?? '';
  const exact = fileNameVisuals.get(name);
  if (exact) return exact;
  if (name.startsWith('.env.') && name.length > 5) return fileNameVisuals.get('.env')!;
  if (/^(?:dockerfile|containerfile)\..+|^.+\.(?:dockerfile|containerfile)$/u.test(name)) {
    return fileNameVisuals.get('dockerfile')!;
  }
  const compound = compoundExtensions.find(extension => name.endsWith(`.${extension}`));
  return extensionVisuals.get(compound ?? getFileExtension(name)) ?? genericVisual;
}
