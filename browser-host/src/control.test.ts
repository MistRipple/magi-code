import assert from "node:assert/strict";
import test from "node:test";
import { ControlFence, ProtocolFailure } from "./control";
import { validateNavigationUrl } from "./host";

test("fence prevents stale agent commands after user takeover", () => {
  const control = new ControlFence();
  control.update(1, "agent");
  control.validate({ mode: "agent", lease_id: "lease-a", fence: 1 });
  control.update(2, "user");

  assert.throws(
    () =>
      control.validate({ mode: "agent", lease_id: "lease-a", fence: 1 }),
    (error) =>
      error instanceof ProtocolFailure && error.code === "browser_lease_fenced",
  );
  control.validate({ mode: "user", fence: 2 });
});

test("fence cannot move backwards", () => {
  const control = new ControlFence();
  control.update(4, "agent");
  assert.throws(
    () => control.update(3, "user"),
    (error) =>
      error instanceof ProtocolFailure &&
      error.code === "browser_fence_regression",
  );
});

test("navigation policy rejects non-web schemes and embedded credentials", () => {
  for (const url of [
    "javascript:alert(1)",
    "data:text/html,blocked",
    "file:///etc/passwd",
    "https://user:password@example.com",
  ]) {
    assert.throws(
      () => validateNavigationUrl(url),
      (error) =>
        error instanceof ProtocolFailure &&
        ["browser_url_scheme_blocked", "browser_url_invalid"].includes(error.code),
      url,
    );
  }
  assert.doesNotThrow(() => validateNavigationUrl("about:blank"));
  assert.doesNotThrow(() => validateNavigationUrl("http://127.0.0.1:38123/web.html"));
  assert.doesNotThrow(() => validateNavigationUrl("https://example.com"));
});
