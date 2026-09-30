import assert from "node:assert/strict";
import test from "node:test";
import {
  browserPartitionForSession,
  isWebModelBrowserSession,
  WEB_MODEL_PARTITION,
  WEB_MODEL_SESSION_ID_PREFIX,
} from "./web-model-session.js";

test("应用级会话 id 被识别，并固定映射到持久分区", () => {
  assert.equal(WEB_MODEL_PARTITION, "persist:magi-web-model");
  for (const id of [
    `${WEB_MODEL_SESSION_ID_PREFIX}1759000000000-0`,
    `${WEB_MODEL_SESSION_ID_PREFIX}1759000000000-7`,
  ]) {
    // 分区名与会话 id 解耦：不同 id 共享同一持久分区，登录态不随 id 丢失。
    assert.equal(isWebModelBrowserSession(id), true);
    assert.equal(browserPartitionForSession(id), WEB_MODEL_PARTITION);
  }
});

test("会话级浏览器 id 不被误判为应用级", () => {
  for (const id of [
    "browser-session-1759000000000-0",
    "browser-tool-session-session-1",
    "",
  ]) {
    assert.equal(isWebModelBrowserSession(id), false);
  }
});

test("普通会话仍使用独立的内存分区，不会污染 GPT Web 持久分区", () => {
  assert.equal(
    browserPartitionForSession("browser-session-1759000000000-0"),
    "magi-browser-browser-session-1759000000000-0",
  );
  assert.notEqual(
    browserPartitionForSession("browser-session-1759000000000-0"),
    WEB_MODEL_PARTITION,
  );
});
