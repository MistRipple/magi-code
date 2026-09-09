import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import Ajv2020 from "ajv/dist/2020.js";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const contractRoot = join(root, "contracts", "desktop-browser");
const schemaFiles = [
  "desktop-ipc.schema.json",
  "desktop-control.schema.json",
  "worker-ipc.schema.json",
  "browser-tool.schema.json",
  "capability-manifest.schema.json",
];
const read = (path) => readFile(join(root, path), "utf8");
const sorted = (values) => [...new Set(values)].sort();

function enumBody(source, enumName, label) {
  const marker = `pub enum ${enumName} {`;
  const start = source.indexOf(marker);
  assert.notEqual(start, -1, `${label} 缺少 ${marker}`);
  let depth = 0;
  let bodyStart = -1;
  for (let index = start; index < source.length; index += 1) {
    if (source[index] === "{") {
      depth += 1;
      if (bodyStart === -1) bodyStart = index + 1;
    } else if (source[index] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(bodyStart, index);
    }
  }
  throw new Error(`${label} ${enumName} 缺少结束括号`);
}

function rustEnumVariants(source, enumName, label) {
  return sorted(
    [...enumBody(source, enumName, label).matchAll(/^    ([A-Z][A-Za-z0-9]*)\s*(?:\{|\(|,)/gmu)].map((match) =>
      match[1].replace(/([a-z0-9])([A-Z])/gu, "$1_$2").toLowerCase(),
    ),
  );
}

function tsStringUnion(source, typeName, label) {
  const match = source.match(new RegExp(`export type ${typeName}\\s*=([\\s\\S]*?);`, "u"));
  assert.ok(match, `${label} 缺少 ${typeName}`);
  return sorted([...match[1].matchAll(/"([^"\\n]+)"/gu)].map((item) => item[1]));
}

function section(source, startMarker, endMarker, label) {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start + startMarker.length);
  assert.ok(start >= 0 && end > start, `${label} 定义不完整`);
  return source.slice(start, end);
}

const schemas = new Map();
for (const file of schemaFiles) {
  const schema = JSON.parse(await readFile(join(contractRoot, file), "utf8"));
  assert.equal(schema.$schema, "https://json-schema.org/draft/2020-12/schema", `${file} 未声明 Draft 2020-12 JSON Schema`);
  assert.equal(typeof schema.$id, "string", `${file} 缺少稳定 schema id`);
  assert.ok(schema.$id.length > 0, `${file} 缺少稳定 schema id`);
  schemas.set(file, schema);
}

const ajv = new Ajv2020({ strictSchema: true, strictTypes: false, allowUnionTypes: true, validateFormats: false });
ajv.addKeyword("x-magi-browser-tool-catalog");
for (const file of schemaFiles) {
  if (file === "worker-ipc.schema.json") continue;
  ajv.compile(schemas.get(file));
}
ajv.compile(schemas.get("worker-ipc.schema.json"));

const browserToolSchema = schemas.get("browser-tool.schema.json");
const browserToolCatalog = browserToolSchema["x-magi-browser-tool-catalog"];
assert.ok(Array.isArray(browserToolCatalog) && browserToolCatalog.length > 0, "浏览器工具目录不能为空");
const browserToolNames = browserToolCatalog.map((entry) => entry.name);
const browserToolVariants = browserToolCatalog.map((entry) => entry.rustVariant);
assert.equal(new Set(browserToolNames).size, browserToolNames.length, "浏览器工具名称不能重复");
assert.equal(new Set(browserToolVariants).size, browserToolVariants.length, "浏览器工具 Rust 变体不能重复");
assert.deepEqual(
  browserToolSchema.$defs.browserToolName.enum,
  browserToolNames,
  "浏览器工具枚举必须与唯一工具目录顺序一致",
);
for (const entry of browserToolCatalog) {
  assert.match(entry.name, /^browser_[a-z0-9_]+$/u, `浏览器工具名称无效: ${entry.name}`);
  assert.match(entry.rustVariant, /^[A-Z][A-Za-z0-9]*$/u, `Rust 变体无效: ${entry.name}`);
  assert.ok(["read", "write", "mixed"].includes(entry.access), `浏览器工具 access 无效: ${entry.name}`);
  assert.equal(typeof entry.description, "string", `浏览器工具缺少描述: ${entry.name}`);
  assert.ok(entry.description.trim(), `浏览器工具描述不能为空: ${entry.name}`);
  assert.equal(entry.inputSchema?.type, "object", `浏览器工具输入必须是对象 Schema: ${entry.name}`);
  ajv.compile(entry.inputSchema);
}

const browserTabsEntry = browserToolCatalog.find((entry) => entry.name === "browser_tabs");
assert.ok(browserTabsEntry, "浏览器工具目录缺少 browser_tabs");
assert.equal(browserTabsEntry.inputSchema.additionalProperties, false, "browser_tabs 不得接受未知字段或子 Tab 身份字段");
assert.match(browserTabsEntry.description, /一级浏览器标签/u, "browser_tabs 必须明确是右栏一级标签");
assert.match(browserTabsEntry.description, /target=_blank/u, "browser_tabs 必须明确网页 popup 不得创建子标签");
for (const forbidden of ["parent_tab_id", "parentTabId", "child_tab_id", "childTabId"]) {
  assert.equal(
    Object.hasOwn(browserTabsEntry.inputSchema.properties ?? {}, forbidden),
    false,
    `browser_tabs 不得暴露 ${forbidden}`,
  );
}

const viewportSchema = browserToolCatalog.find((entry) => entry.name === "browser_viewport")?.inputSchema;
assert.ok(viewportSchema, "浏览器工具目录缺少 browser_viewport");
const validateViewport = ajv.compile(viewportSchema);
for (const [input, expected] of [
  [{ action: "get" }, true],
  [{ action: "get", mode: "auto" }, false],
  [{ action: "set" }, false],
  [{ action: "set", mode: "auto" }, true],
  [{ action: "set", mode: "auto", width: 800, height: 600 }, false],
  [{ action: "set", mode: "fixed", width: 800, height: 600 }, true],
  [{ action: "set", mode: "fixed", width: 800 }, false],
]) {
  assert.equal(
    validateViewport(input),
    expected,
    `browser_viewport Schema 分支校验错误: ${JSON.stringify(input)}`,
  );
}

const generatedBrowserCatalogRust = await read("crates/magi-browser-authority/src/browser_tool_catalog.generated.rs");
const generatedBrowserCatalogTs = await read("contracts/desktop-browser/src/browser-tool-catalog.generated.ts");
const browserCapability = await read("crates/magi-browser-authority/src/capability.rs");
const builtinCatalog = await read("crates/magi-tool-runtime/src/builtin_catalog.rs");
const browserRuntime = await read("crates/magi-api/src/browser_tool_runtime.rs");
const appServer = await read("crates/magi-api/src/app_server.rs");
const appServerSchema = JSON.parse(await read("contracts/app-server/app-server.schema.json"));

assert.deepEqual(
  rustEnumVariants(generatedBrowserCatalogRust, "BrowserToolKind", "生成的 Rust BrowserToolKind"),
  sorted(browserToolVariants.map((variant) => variant.replace(/([a-z0-9])([A-Z])/gu, "$1_$2").toLowerCase())),
  "浏览器工具唯一目录与生成的 Rust 工具枚举不一致",
);
for (const entry of browserToolCatalog) {
  assert.ok(generatedBrowserCatalogTs.includes(`name: ${JSON.stringify(entry.name)}`), `生成的 TypeScript 目录缺少 ${entry.name}`);
  assert.ok(generatedBrowserCatalogRust.includes(`Self::${entry.rustVariant} => {`), `生成的 Rust 目录缺少 ${entry.rustVariant}`);
}
assert.doesNotMatch(browserCapability, /pub enum BrowserToolKind/u, "BrowserToolKind 不得在 capability.rs 保留第二份定义");
assert.doesNotMatch(browserCapability, /pub enum BrowserToolAccess/u, "BrowserToolAccess 不得在 capability.rs 保留第二份定义");
assert.match(builtinCatalog, /BrowserToolKind::from_name\(self\.as_str\(\)\)/u, "内置工具必须通过唯一目录解析浏览器工具");
assert.match(builtinCatalog, /return tool\.input_schema\(\);/u, "内置工具参数 Schema 必须来自唯一目录");
assert.doesNotMatch(builtinCatalog, /Self::Browser[A-Za-z]+ => serde_json::json!/u, "内置工具不得保留浏览器参数 Schema 副本");
assert.match(browserRuntime, /BrowserToolKind::from_name\(tool_name\)/u, "浏览器运行时必须通过唯一目录解析工具名");
assert.match(browserRuntime, /let Some\(mode\) = optional_string\(arguments, "mode"\)/u, "browser_viewport set 必须显式指定 mode");
assert.doesNotMatch(browserRuntime, /optional_string\(arguments, "mode"\)\.unwrap_or_else\(\|\|\s*"fixed"/u, "browser_viewport 不得保留省略 mode 的兼容默认值");
assert.match(appServer, /"description": tool\.description\(\)/u, "App Server 工具目录必须返回统一描述");
assert.match(appServer, /"inputSchema": tool\.input_schema\(\)/u, "App Server 工具目录必须返回统一输入 Schema");
assert.deepEqual(
  appServerSchema.$defs.BrowserToolDescriptor.required,
  ["name", "access", "description", "inputSchema"],
  "App Server 浏览器工具描述符必须包含完整工具元数据",
);

const desktopIndex = await read("apps/desktop/src/main/index.ts");
const ipcChannels = sorted(
  [...desktopIndex.matchAll(/handleIpc\(\s*"([^"]+)"/gu)].map(
    (match) => match[1],
  ),
);
const schemaIpcChannels = sorted(schemas.get("desktop-ipc.schema.json").properties.channel.enum);
assert.deepEqual(schemaIpcChannels, ipcChannels, "Desktop IPC Schema/实际 handler 通道集合不一致");

const controlSchema = schemas.get("desktop-control.schema.json");
const typescript = await read("contracts/desktop-browser/src/index.ts");
const rustHostProtocol = await read("crates/magi-browser-authority/src/host_protocol.rs");
const rustDomain = await read("crates/magi-browser-authority/src/domain.rs");

const tsVersion = typescript.match(/DESKTOP_BROWSER_PROTOCOL_VERSION\s*=\s*\{\s*major:\s*(\d+)\s*,\s*minor:\s*(\d+)\s*\}/u);
const rustMajor = rustHostProtocol.match(/BROWSER_HOST_PROTOCOL_MAJOR:\s*u16\s*=\s*(\d+)/u);
const rustMinor = rustHostProtocol.match(/BROWSER_HOST_PROTOCOL_MINOR:\s*u16\s*=\s*(\d+)/u);
assert.ok(tsVersion && rustMajor && rustMinor, "无法读取 Desktop Browser 协议版本");
assert.equal(Number(tsVersion[1]), Number(rustMajor[1]), "TypeScript/Rust major 版本不一致");
assert.equal(Number(tsVersion[2]), Number(rustMinor[1]), "TypeScript/Rust minor 版本不一致");

const rawSchemaCommands = controlSchema.$defs.command.oneOf.map((branch) => branch.properties.type.const);
assert.equal(rawSchemaCommands.length, new Set(rawSchemaCommands).size, "desktop-control.schema.json 命令重复");
const schemaCommands = sorted(rawSchemaCommands);
const tsCommandSection = section(
  typescript,
  "export type BrowserHostCommand =",
  "export interface BrowserHostRequestEnvelope",
  "TypeScript BrowserHostCommand",
);
const tsCommands = sorted(
  [...tsCommandSection.matchAll(/\btype:\s*((?:"[a-z_]+"(?:\s*\|\s*)?)+)/gu)].flatMap((match) =>
    [...match[1].matchAll(/"([a-z_]+)"/gu)].map((item) => item[1]),
  ),
);
const rustCommands = rustEnumVariants(rustHostProtocol, "BrowserHostCommand", "Rust BrowserHostCommand");
assert.deepEqual(schemaCommands, tsCommands, "Schema/TypeScript 命令集合不一致");
assert.deepEqual(schemaCommands, rustCommands, "Schema/Rust 命令集合不一致");
assert.equal(schemaCommands.length, 23, "Desktop Browser 命令集合数量发生漂移");

for (const branch of controlSchema.$defs.command.oneOf) {
  const command = branch.properties.type.const;
  const hasPayload = Object.hasOwn(branch.properties, "payload");
  if (command === "ping" || command === "shutdown") {
    assert.equal(hasPayload, false, `${command} 不应声明 payload`);
    continue;
  }
  assert.equal(hasPayload, true, `${command} 缺少 payload Schema`);
  assert.ok(branch.required.includes("payload"), `${command} 必须要求 payload`);
  const ref = branch.properties.payload.$ref;
  assert.match(ref, /^#\/\$defs\/[A-Za-z][A-Za-z0-9]*$/u, `${command} payload 必须引用命名 Schema`);
  const payload = controlSchema.$defs[ref.slice("#/$defs/".length)];
  assert.ok(payload, `${command} 引用了不存在的 payload Schema`);
  assert.equal(payload.type, "object", `${command} payload 必须是对象`);
  assert.equal(payload.additionalProperties, false, `${command} payload 必须禁止未知字段`);
  assert.ok(Array.isArray(payload.required) && payload.required.length > 0, `${command} payload 必须声明 required`);
}

const annotations = controlSchema.$defs.annotationsPayload;
assert.deepEqual(annotations.required, ["tab_id", "annotations"], "set_annotations payload required 不完整");
assert.equal(annotations.properties.annotations.type, "array", "set_annotations.annotations 必须是数组");
assert.deepEqual(annotations.properties.annotations.items, { $ref: "#/$defs/jsonValue" }, "set_annotations.annotations 必须保持 JSON value 数组语义");

const surfaceIdentity = controlSchema.$defs.surfaceIdentity;
assert.deepEqual(
  surfaceIdentity.required,
  ["tab_id", "surface_id", "navigation_revision"],
  "inspect payload 必须携带完整 Surface 身份",
);
assert.equal(surfaceIdentity.additionalProperties, false, "inspect payload 不得接受未知身份字段");

const deviceTypes = sorted(controlSchema.$defs.logicalViewport.oneOf[1].properties.device_type.enum);
assert.deepEqual(deviceTypes, tsStringUnion(typescript, "BrowserDeviceType", "TypeScript BrowserDeviceType"), "Schema/TypeScript device_type 枚举不一致");
assert.deepEqual(deviceTypes, rustEnumVariants(rustDomain, "BrowserDeviceType", "Rust BrowserDeviceType"), "Schema/Rust device_type 枚举不一致");

const navigationSection = section(typescript, "export type BrowserNavigation =", "export type BrowserHostCommand =", "TypeScript BrowserNavigation");
const tsNavigationActions = sorted([...navigationSection.matchAll(/action:\s*"([^"\n]+)"/gu)].map((match) => match[1]));
const schemaNavigationActions = sorted(controlSchema.$defs.navigation.oneOf.map((branch) => branch.properties.action.const));
assert.deepEqual(schemaNavigationActions, tsNavigationActions, "Schema/TypeScript navigation.action 枚举不一致");
assert.deepEqual(schemaNavigationActions, rustEnumVariants(rustHostProtocol, "BrowserNavigation", "Rust BrowserNavigation"), "Schema/Rust navigation.action 枚举不一致");

const controlSection = section(typescript, "export type BrowserControl =", "export type BrowserControlUpdate", "TypeScript BrowserControl");
const tsControlModes = sorted([...controlSection.matchAll(/mode:\s*"([^"\n]+)"/gu)].map((match) => match[1]));
const schemaControlModes = sorted(controlSchema.$defs.control.oneOf.map((branch) => branch.properties.mode.const));
assert.deepEqual(schemaControlModes, tsControlModes, "Schema/TypeScript control.mode 枚举不一致");
assert.deepEqual(schemaControlModes, rustEnumVariants(rustHostProtocol, "BrowserHostControl", "Rust BrowserHostControl"), "Schema/Rust control.mode 枚举不一致");

const updateSection = section(typescript, "export type BrowserControlUpdate =", "export interface BrowserSnapshotTarget", "TypeScript BrowserControlUpdate");
const tsControlUpdateModes = sorted([...updateSection.matchAll(/mode:\s*"([^"\n]+)"/gu)].map((match) => match[1]));
const schemaControlUpdateModes = sorted(controlSchema.$defs.controlUpdate.oneOf.map((branch) => branch.properties.mode.const));
assert.deepEqual(schemaControlUpdateModes, tsControlUpdateModes, "Schema/TypeScript control update mode 枚举不一致");
assert.deepEqual(schemaControlUpdateModes, rustEnumVariants(rustHostProtocol, "BrowserHostControlUpdate", "Rust BrowserHostControlUpdate"), "Schema/Rust control update mode 枚举不一致");

assert.deepEqual(
  sorted(controlSchema.$defs.screenshotPayload.properties.format.enum),
  rustEnumVariants(rustHostProtocol, "BrowserScreenshotFormat", "Rust BrowserScreenshotFormat"),
  "Schema/Rust screenshot.format 枚举不一致",
);

process.stdout.write(`Desktop Browser contracts 校验通过，协议 ${tsVersion[1]}.${tsVersion[2]}，命令 ${schemaCommands.length} 个，payload Schema ${schemaCommands.length - 2} 个。\n`);
