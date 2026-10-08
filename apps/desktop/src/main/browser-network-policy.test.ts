import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  evaluateBrowserNavigation,
  evaluateBrowserRequestTarget,
  type BrowserNetworkPolicyConfig,
} from "./browser-network-policy.js";

/** 与 daemon 共用的规则与用例，见 contracts/desktop-browser/network-policy.json。 */
const source = JSON.parse(
  readFileSync(
    new URL("../../../../contracts/desktop-browser/network-policy.json", import.meta.url),
    "utf8",
  ),
) as { vectors: Record<string, string[]> };

function vectors(name: string): string[] {
  const values = source.vectors[name];
  assert.ok(values && values.length > 0, `vectors.${name} must not be empty`);
  return values;
}

const open: BrowserNetworkPolicyConfig = { selfPorts: new Set(), allowPrivateNetwork: true };
const closed: BrowserNetworkPolicyConfig = { selfPorts: new Set(), allowPrivateNetwork: false };

function verdict(raw: string, config: BrowserNetworkPolicyConfig) {
  return evaluateBrowserNavigation(new URL(raw), config);
}

test("共享用例：云元数据与链路本地地址始终被拦截", () => {
  for (const raw of vectors("alwaysBlocked")) {
    assert.deepEqual(verdict(raw, open), { allowed: false, reason: "blocked_target" }, raw);
  }
});

test("共享用例：公网目标放行", () => {
  for (const raw of [...vectors("allowed"), ...vectors("allowedEvenWhenPrivateNetworkDisallowed")]) {
    assert.deepEqual(verdict(raw, open), { allowed: true }, raw);
    assert.deepEqual(verdict(raw, closed), { allowed: true }, raw);
  }
});

test("共享用例：关闭本机与局域网后私有目标被拦截，打开时放行", () => {
  for (const raw of vectors("blockedWhenPrivateNetworkDisallowed")) {
    assert.deepEqual(verdict(raw, open), { allowed: true }, raw);
    assert.deepEqual(verdict(raw, closed), { allowed: false, reason: "private_network" }, raw);
  }
});

test("Magi 自身端口只在本机地址上被拦截", () => {
  const withSelf: BrowserNetworkPolicyConfig = { selfPorts: new Set([38123]), allowPrivateNetwork: true };
  for (const raw of [
    "http://127.0.0.1:38123/web.html",
    "http://localhost:38123/api/session/tool-approval",
    "http://[::1]:38123/",
    "http://127.1:38123/",
    "http://0.0.0.0:38123/",
  ]) {
    assert.deepEqual(verdict(raw, withSelf), { allowed: false, reason: "self_origin" }, raw);
  }
  assert.deepEqual(
    evaluateBrowserRequestTarget(new URL("ws://127.0.0.1:38123/events"), withSelf),
    { allowed: false, reason: "self_origin" },
    "WebSocket 请求同样被拦截",
  );
  for (const raw of ["http://127.0.0.1:3000/", "https://example.com:38123/", "http://192.168.1.2:38123/"]) {
    assert.deepEqual(verdict(raw, withSelf), { allowed: true }, raw);
  }
});

test("协议与凭据只约束导航入口，不约束子资源请求", () => {
  assert.deepEqual(verdict("file:///etc/passwd", open), { allowed: false, reason: "unsupported_protocol" });
  assert.deepEqual(verdict("javascript:alert(1)", open), { allowed: false, reason: "unsupported_protocol" });
  assert.deepEqual(verdict("https://user:pass@example.com/", open), { allowed: false, reason: "credentials" });
  assert.deepEqual(verdict("about:blank", open), { allowed: true });
  assert.deepEqual(
    evaluateBrowserRequestTarget(new URL("https://user:pass@example.com/"), open),
    { allowed: true },
  );
});
