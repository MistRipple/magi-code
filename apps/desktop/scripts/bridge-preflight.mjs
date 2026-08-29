import { strict as assert } from "node:assert";
import { constants } from "node:fs";
import { access, stat } from "node:fs/promises";
import { spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const bridgeBinaryNames = Object.freeze([
  "model_bridge_loopback",
  "mcp_bridge_loopback",
]);

const BRIDGE_PROTOCOL_VERSION = "local-bridge-v1";
const PREFLIGHT_PROMPT = "electron-release-preflight";
const PREFLIGHT_TIMEOUT_MS = 15_000;
const MAX_OUTPUT_BYTES = 4 * 1024 * 1024;

export function bridgeBinaryFileName(binaryName) {
  if (!bridgeBinaryNames.includes(binaryName)) {
    throw new Error(`未知 bridge binary: ${binaryName}`);
  }
  return `${binaryName}${process.platform === "win32" ? ".exe" : ""}`;
}

export function bridgeBinaryPath(root, binaryName) {
  return join(root, bridgeBinaryFileName(binaryName));
}

export async function assertBridgeBinaries(root, label = "bridge") {
  const paths = Object.fromEntries(
    bridgeBinaryNames.map((binaryName) => [binaryName, bridgeBinaryPath(root, binaryName)]),
  );
  await Promise.all(
    Object.entries(paths).map(([binaryName, path]) =>
      assertExecutableFile(path, `${label} ${binaryName}`)),
  );
  return paths;
}

export async function runBridgePreflight(root, label = "bridge") {
  const paths = await assertBridgeBinaries(root, label);
  await probeModelBridge(paths.model_bridge_loopback);
  await probeMcpBridge(paths.mcp_bridge_loopback);
  return paths;
}

async function assertExecutableFile(path, label) {
  let metadata;
  try {
    metadata = await stat(path);
  } catch (cause) {
    throw new Error(`${label} 不存在: ${path}`, { cause });
  }
  if (!metadata.isFile()) throw new Error(`${label} 不是文件: ${path}`);
  try {
    await access(path, constants.X_OK);
  } catch (cause) {
    throw new Error(`${label} 不可执行: ${path}`, { cause });
  }
  if (process.platform !== "win32" && (metadata.mode & 0o111) === 0) {
    throw new Error(`${label} 缺少执行权限: ${path}`);
  }
  if (process.platform === "win32" && !path.toLowerCase().endsWith(".exe")) {
    throw new Error(`${label} 必须是 .exe: ${path}`);
  }
}

async function probeModelBridge(binaryPath) {
  const handshake = await callBridge(binaryPath, "bridge.handshake", null);
  assert.equal(handshake.protocol_version, BRIDGE_PROTOCOL_VERSION);
  assert.equal(handshake.server_kind, "model");
  assert.ok(handshake.supported_methods?.includes("model.invoke"));

  const health = await callBridge(binaryPath, "bridge.health", null);
  assert.equal(health.protocol_version, BRIDGE_PROTOCOL_VERSION);
  assert.equal(health.server_kind, "model");
  assert.equal(health.ok, true);

  const catalog = await callBridge(binaryPath, "bridge.describe_services", null);
  assert.equal(catalog.protocol_version, BRIDGE_PROTOCOL_VERSION);
  assert.equal(catalog.server_kind, "model");
  assert.ok(Array.isArray(catalog.services) && catalog.services.length > 0);

  const response = await callBridge(binaryPath, "model.invoke", {
    provider: "loopback-model",
    prompt: PREFLIGHT_PROMPT,
  });
  assert.equal(response.status, "completed");
  assert.equal(response.content, `loopback-model::${PREFLIGHT_PROMPT}`);
}

async function probeMcpBridge(binaryPath) {
  const handshake = await callBridge(binaryPath, "bridge.handshake", null);
  assert.equal(handshake.protocol_version, BRIDGE_PROTOCOL_VERSION);
  assert.equal(handshake.server_kind, "mcp");
  assert.ok(handshake.supported_methods?.includes("mcp.list_servers"));
  assert.ok(handshake.supported_methods?.includes("mcp.call_tool"));

  const health = await callBridge(binaryPath, "bridge.health", null);
  assert.equal(health.protocol_version, BRIDGE_PROTOCOL_VERSION);
  assert.equal(health.server_kind, "mcp");
  assert.equal(health.ok, true);

  const catalog = await callBridge(binaryPath, "bridge.describe_services", null);
  assert.equal(catalog.protocol_version, BRIDGE_PROTOCOL_VERSION);
  assert.equal(catalog.server_kind, "mcp");
  assert.ok(Array.isArray(catalog.services) && catalog.services.length > 0);

  const servers = await callBridge(binaryPath, "mcp.list_servers", null);
  assert.equal(servers.default_route_status, "ready");
  assert.equal(servers.default_route_target, "loopback-mcp");

  const response = await callBridge(binaryPath, "mcp.call_tool", {
    server_name: "loopback-mcp",
    tool_name: "echo.inspect",
    input: JSON.stringify({ message: PREFLIGHT_PROMPT }),
  });
  assert.equal(response.ok, true);
  const payload = JSON.parse(response.payload);
  assert.equal(payload.status, "ok");
  assert.equal(payload.server_name, "loopback-mcp");
  assert.equal(payload.tool_name, "echo.inspect");
}

function callBridge(binaryPath, method, params) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(binaryPath, [], {
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    let settled = false;
    let timer;

    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      if (error) rejectPromise(error);
      else resolvePromise(value);
    };

    timer = setTimeout(() => {
      child.kill();
      finish(new Error(`bridge preflight 超时: ${binaryPath} ${method}`));
    }, PREFLIGHT_TIMEOUT_MS);

    child.stdout.on("data", (chunk) => {
      if (Buffer.byteLength(stdout) < MAX_OUTPUT_BYTES) stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      if (Buffer.byteLength(stderr) < MAX_OUTPUT_BYTES) stderr += chunk.toString();
    });
    child.once("error", (error) => finish(new Error(
      `bridge preflight 启动失败: ${binaryPath} ${method}`,
      { cause: error },
    )));
    child.once("close", (code, signal) => {
      if (settled) return;
      if (code !== 0) {
        finish(new Error(
          `bridge preflight 进程失败: ${binaryPath} ${method}; `
          + `exit=${code ?? signal ?? "unknown"}; stderr=${stderr.trim()}`,
        ));
        return;
      }
      try {
        const lines = stdout.trim().split(/\r?\n/).filter(Boolean);
        const raw = lines.at(-1);
        if (!raw) throw new Error("stdout 为空");
        const envelope = JSON.parse(raw);
        if (envelope.jsonrpc !== "2.0" || envelope.id !== 1) {
          throw new Error("JSON-RPC 响应头无效");
        }
        if (envelope.error) {
          throw new Error(`${envelope.error.code}: ${envelope.error.message}`);
        }
        if (!envelope.result || typeof envelope.result !== "object") {
          throw new Error("JSON-RPC result 无效");
        }
        finish(null, envelope.result);
      } catch (error) {
        finish(new Error(
          `bridge preflight 响应无效: ${binaryPath} ${method}`,
          { cause: error },
        ));
      }
    });
    child.stdin.once("error", (error) => finish(new Error(
      `bridge preflight 请求写入失败: ${binaryPath} ${method}`,
      { cause: error },
    )));
    child.stdin.end(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method, params })}\n`);
  });
}

async function runCli() {
  const args = process.argv.slice(2);
  let profile = "debug";
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] !== "--profile") throw new Error(`不支持的参数: ${args[index]}`);
    profile = args[++index];
    if (profile !== "debug" && profile !== "release") {
      throw new Error(`bridge preflight profile 无效: ${profile}`);
    }
  }
  const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
  await runBridgePreflight(join(repositoryRoot, "target", profile), `Rust ${profile} bridge`);
  process.stdout.write(`Rust ${profile} bridge 实际 preflight 通过。\n`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await runCli();
}
