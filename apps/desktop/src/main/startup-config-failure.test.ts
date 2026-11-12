import assert from "node:assert/strict";
import { test } from "node:test";
import {
  diagnoseUnsupportedStateLayout,
  diagnoseUserStateFileStartupFailure,
} from "./startup-config-failure.js";

test("recognizes a damaged JSON state file under the Magi state root", () => {
  const result = diagnoseUserStateFileStartupFailure(
    "magi_daemon_start_failed",
    "持久状态文件损坏，拒绝以空状态继续 /Users/test/.magi/workspaces.json: unknown field at line 1 column 4",
    "/Users/test/.magi",
  );

  assert.deepEqual(result, { filePath: "/Users/test/.magi/workspaces.json" });
});

test("does not treat a daemon identity mismatch as a configuration file failure", () => {
  const result = diagnoseUserStateFileStartupFailure(
    "magi_daemon_identity_mismatch:product_version_mismatch",
    "daemon health identity mismatch",
    "/Users/test/.magi",
  );

  assert.equal(result, null);
});

test("does not suggest clearing a project state file outside the global state root", () => {
  const result = diagnoseUserStateFileStartupFailure(
    "magi_daemon_start_failed",
    "持久状态文件损坏，拒绝以空状态继续 /Users/test/project/.magi/workspaces.json: invalid type",
    "/Users/test/.magi",
  );

  assert.equal(result, null);
});

test("recognizes an unsupported state layout reported by the daemon", () => {
  const result = diagnoseUnsupportedStateLayout(
    "magi_daemon_start_failed:magi_daemon_exited:1",
    'Error: Internal("不支持的 state layout 版本: 21")\nError: Internal("不支持的 state layout 版本: 21")',
  );

  assert.deepEqual(result, { layoutVersion: 21 });
});

test("does not treat other startup failures as an unsupported state layout", () => {
  assert.equal(
    diagnoseUnsupportedStateLayout("magi_daemon_start_failed", "port already in use"),
    null,
  );
});
