import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { defaultStateRoot } from "./state-root.js";

test("默认状态根与开发 daemon 和历史客户端共用 ~/.magi", () => {
  assert.equal(
    defaultStateRoot("/Users/test"),
    join("/Users/test", ".magi"),
  );
  assert.equal(defaultStateRoot("  /tmp/home  "), join("/tmp/home", ".magi"));
});
