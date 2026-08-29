import assert from "node:assert/strict";
import test from "node:test";

import { normalizeOptionalDomNodeId } from "@magi/desktop-browser-contracts";

test("可选 DOM.nodeId 为 0 时在 Chromium 事件边界规范化为 null", () => {
  assert.equal(normalizeOptionalDomNodeId(0), null);
});

test("可选 DOM.nodeId 只接受安全正整数", () => {
  assert.equal(normalizeOptionalDomNodeId(42), 42);
  assert.equal(normalizeOptionalDomNodeId(-1), null);
  assert.equal(normalizeOptionalDomNodeId(1.5), null);
  assert.equal(normalizeOptionalDomNodeId(Number.NaN), null);
  assert.equal(normalizeOptionalDomNodeId("42"), null);
  assert.equal(normalizeOptionalDomNodeId(Number.MAX_SAFE_INTEGER + 1), null);
});
