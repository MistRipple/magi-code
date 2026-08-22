import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const contractRoot = join(root, "contracts", "desktop-browser");
const schemaFiles = [
  "desktop-ipc.schema.json",
  "desktop-control.schema.json",
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
    [...enumBody(source, enumName, label).matchAll(/^    ([A-Z][A-Za-z0-9]*)\s*(?:\{|,)/gmu)].map((match) =>
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
  tsCommandSection
    .split("\n")
    .filter((line) => /(?:^|\{)\s*type:\s*/u.test(line))
    .flatMap((line) => [...line.matchAll(/"([a-z_]+)"/gu)].map((match) => match[1])),
);
const rustCommands = rustEnumVariants(rustHostProtocol, "BrowserHostCommand", "Rust BrowserHostCommand");
assert.deepEqual(schemaCommands, tsCommands, "Schema/TypeScript 命令集合不一致");
assert.deepEqual(schemaCommands, rustCommands, "Schema/Rust 命令集合不一致");
assert.equal(schemaCommands.length, 19, "Desktop Browser 命令集合数量发生漂移");

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
