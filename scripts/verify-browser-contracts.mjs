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

for (const file of schemaFiles) {
  const source = await readFile(join(contractRoot, file), "utf8");
  const schema = JSON.parse(source);
  if (schema.$schema !== "https://json-schema.org/draft/2020-12/schema") {
    throw new Error(`${file} 未声明 Draft 2020-12 JSON Schema`);
  }
  if (typeof schema.$id !== "string" || schema.$id.length === 0) {
    throw new Error(`${file} 缺少稳定 schema id`);
  }
}

const typescript = await readFile(join(contractRoot, "src", "index.ts"), "utf8");
const protocol = typescript.match(
  /DESKTOP_BROWSER_PROTOCOL_VERSION\s*=\s*\{\s*major:\s*(\d+)\s*,\s*minor:\s*(\d+)\s*\}/u,
);
if (!protocol) throw new Error("无法读取 Desktop Browser 协议版本");

const controlSchema = JSON.parse(
  await readFile(join(contractRoot, "desktop-control.schema.json"), "utf8"),
);
const commandTypes = controlSchema.$defs?.command?.properties?.type?.enum ?? [];
for (const requiredType of ["get_logical_viewport", "set_logical_viewport", "screenshot", "update_control"]) {
  if (!commandTypes.includes(requiredType)) {
    throw new Error(`desktop-control.schema.json 缺少命令 ${requiredType}`);
  }
}

process.stdout.write(
  `Desktop Browser contracts 校验通过，协议 ${protocol[1]}.${protocol[2]}。\n`,
);
