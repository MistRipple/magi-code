import assert from "node:assert/strict";
import test from "node:test";
import { sanitizePlatformSetupResult } from "./web-platform-setup.js";

test("平台配置只返回固定状态与合法 Tunnel id，任意页面正文和密钥不会透传", () => {
  assert.deepEqual(sanitizePlatformSetupResult({ status: "ready", tunnel_id: "tunnel_abc-123", api_key: "sk-secret", text: "private page" }), {
    status: "ready", tunnel_id: "tunnel_abc-123",
  });
  assert.deepEqual(sanitizePlatformSetupResult({ status: "login_required", tunnel_id: "tunnel_hidden", api_key: "sk-secret" }), { status: "login_required" });
  for (const tunnel_id of ["", "sk-secret", "tunnel_a b", "tunnel_" + "x".repeat(161)]) {
    assert.equal(sanitizePlatformSetupResult({ status: "ready", tunnel_id }), null);
  }
  for (const value of [null, 1, {}, { status: "SECRET_PAGE_TEXT" }]) assert.equal(sanitizePlatformSetupResult(value), null);
});
