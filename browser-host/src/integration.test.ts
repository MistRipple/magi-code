import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { WebSocket } from "ws";
import { startBrowserHostServer } from "./index";
import type {
  EventEnvelope,
  HostSnapshot,
  RequestEnvelope,
  ResponseEnvelope,
} from "./protocol";
import { PROTOCOL_VERSION } from "./protocol";

const chromiumExecutable = process.env.MAGI_BROWSER_TEST_CHROMIUM;

function pngDimensions(bytes: Buffer): { width: number; height: number } {
  assert.deepEqual(
    [...bytes.subarray(0, 8)],
    [137, 80, 78, 71, 13, 10, 26, 10],
  );
  return {
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20),
  };
}

function jpegDimensions(bytes: Buffer): { width: number; height: number } {
  const startOfFrameMarkers = new Set([
    0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7,
    0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
  ]);
  let offset = 2;
  while (offset + 9 < bytes.length) {
    if (bytes[offset] !== 0xff) {
      offset += 1;
      continue;
    }
    const marker = bytes[offset + 1];
    offset += 2;
    if (marker === 0xd8 || marker === 0xd9) continue;
    const segmentLength = bytes.readUInt16BE(offset);
    if (startOfFrameMarkers.has(marker)) {
      return {
        width: bytes.readUInt16BE(offset + 5),
        height: bytes.readUInt16BE(offset + 3),
      };
    }
    offset += segmentLength;
  }
  throw new Error("JPEG frame does not contain dimensions");
}

test(
  "real Chromium executes the private Host protocol end to end",
  { skip: !chromiumExecutable, timeout: 60_000 },
  async () => {
    const profilePath = await mkdtemp(join(tmpdir(), "magi-browser-host-"));
    let signalSlowRequestStarted: (() => void) | undefined;
    const slowRequestStarted = new Promise<void>((accept) => {
      signalSlowRequestStarted = accept;
    });
    const appServer = createServer((request, response) => {
      if (request.url === "/drop") {
        request.socket.destroy();
        return;
      }
      if (request.url === "/redirect") {
        const address = appServer.address();
        assert(address && typeof address !== "string");
        response
          .writeHead(302, { location: `http://localhost:${address.port}/` })
          .end();
        return;
      }
      if (request.url === "/slow") {
        signalSlowRequestStarted?.();
        setTimeout(() => {
          response
            .writeHead(200, { "content-type": "text/html; charset=utf-8" })
            .end("<!doctype html><title>Slow page</title>");
        }, 2_000);
        return;
      }
      if (request.url === "/responsive") {
        const mobile = /Android/.test(request.headers["user-agent"] ?? "");
        response
          .writeHead(200, { "content-type": "text/html; charset=utf-8" })
          .end(`<!doctype html>
<html>
  <head>
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Responsive device test</title>
    <style>
      body {
        min-height: 2_000px;
      }
      #responsive-grid {
        display: grid;
        grid-template-columns: repeat(3, minmax(0, 1fr));
        gap: 8px;
      }
      @media (max-width: 600px) {
        #responsive-grid {
          grid-template-columns: 1fr;
        }
      }
    </style>
  </head>
  <body>
    <p>${mobile ? "Server mobile" : "Server desktop"}</p>
    <p id="client-state"></p>
    <div id="responsive-grid">
      <p>Responsive A</p>
      <p>Responsive B</p>
      <p>Responsive C</p>
    </div>
    <script>
      let resizeCount = 0;
      const renderClientState = () => {
        document.querySelector('#client-state').textContent = [
          'Viewport ' + window.innerWidth,
          matchMedia('(max-width: 600px)').matches ? 'Media mobile' : 'Media desktop',
          'Touch ' + navigator.maxTouchPoints,
          'Resize ' + resizeCount,
        ].join(' / ');
      };
      window.addEventListener('resize', () => {
        resizeCount += 1;
        renderClientState();
      });
      renderClientState();
    </script>
  </body>
</html>`);
        return;
      }
      if (request.url === "/download") {
        response
          .writeHead(200, {
            "content-type": "text/plain; charset=utf-8",
            "content-disposition": 'attachment; filename="sample.txt"',
          })
          .end("Magi browser download");
        return;
      }
      if (request.url === "/popup") {
        response
          .writeHead(200, { "content-type": "text/html; charset=utf-8" })
          .end("<!doctype html><title>Popup</title><p>Popup content</p>");
        return;
      }
      response
        .writeHead(200, { "content-type": "text/html; charset=utf-8" })
        .end(`<!doctype html>
<html>
  <body>
    <button id="increment">Increment</button>
    <button id="delete-account">Delete account</button>
    <button id="show-dialog" onclick="alert('Dialog content')">Show dialog</button>
    <button id="open-popup" onclick="window.open('/popup', '_blank')">Open popup</button>
    <a id="download" href="/download" download="sample.txt">Download sample</a>
    <input aria-label="Upload file" type="file" />
    <input aria-label="Name" />
    <input aria-label="Password" type="password" value="server-secret" />
    <input aria-label="Verification code" autocomplete="one-time-code" value="123456" />
    <input aria-label="Card number" autocomplete="cc-number" value="4111111111111111" />
    <a id="redirect" href="/redirect">Redirect</a>
    <p id="count">Count 0</p>
    <script>
      let count = 0;
      document.querySelector('#increment').addEventListener('click', () => {
        count += 1;
        document.querySelector('#count').textContent = 'Count ' + count;
      });
    </script>
  </body>
</html>`);
    });
    await new Promise<void>((accept) =>
      appServer.listen(0, "127.0.0.1", () => accept()),
    );
    const appAddress = appServer.address();
    assert(appAddress && typeof appAddress !== "string");
    const token = "browser-host-integration-token-000000000000";
    const downloadPath = join(profilePath, "downloads");
    let hostServer: Awaited<ReturnType<typeof startBrowserHostServer>> | undefined;
    let client: HostTestClient | undefined;

    try {
      hostServer = await startBrowserHostServer({
        profilePath,
        chromiumExecutable: chromiumExecutable!,
        runtimeVersion: "test-runtime",
        hostVersion: "0.1.0",
        playwrightVersion: "1.58.2",
        runtimeEpoch: 1,
        headless: true,
        deviceScaleFactor: 2,
        downloadPath,
        maxActivePages: 16,
        maxTabs: 16,
        bindHost: "127.0.0.1",
        port: 0,
        authToken: token,
      });
      if (process.platform !== "win32") {
        const browserCommands = execFileSync("/bin/ps", ["-axo", "command"], {
          encoding: "utf8",
        })
          .split("\n")
          .filter((line) => line.includes(profilePath));
        assert(browserCommands.length > 0, "sandbox test must locate the Chromium process");
        assert(
          browserCommands.every((line) => !line.includes("--no-sandbox")),
          "Browser Runtime must never disable the Chromium sandbox",
        );
      }
      client = new HostTestClient(
        `ws://127.0.0.1:${hostServer.port}/control`,
        token,
      );
      await client.open();
      await client.waitForEvent("ready");
      await client.call({
        type: "update_control",
        payload: { fence: 1, mode: "agent" },
      });
      const failedPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-create-failure",
          initial_url: `http://127.0.0.1:${appAddress.port}/drop`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(failedPage.outcome.status, "failed");
      const retriedPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-create-failure",
          initial_url: "about:blank",
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(retriedPage.outcome.status, "succeeded");
      const slowPagePromise = client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-slow",
          initial_url: `http://127.0.0.1:${appAddress.port}/slow`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      await slowRequestStarted;
      const fastPageStartedAt = performance.now();
      const fastPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-fast",
          initial_url: "about:blank",
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(fastPage.outcome.status, "succeeded");
      assert(
        performance.now() - fastPageStartedAt < 1_000,
        "a slow page must not block commands for another tab",
      );
      assert.equal((await slowPagePromise).outcome.status, "succeeded");
      const restoredPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-restored",
          initial_url: `http://127.0.0.1:${appAddress.port}/`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 41,
          snapshot_revision: 17,
        },
      });
      assert.equal(restoredPage.outcome.status, "succeeded");
      if (restoredPage.outcome.status === "succeeded") {
        assert.equal(restoredPage.outcome.payload.type, "page_state");
        if (restoredPage.outcome.payload.type === "page_state") {
          assert.equal(restoredPage.outcome.payload.payload.navigation_revision, 41);
        }
      }
      const restoredSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-restored",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert.equal(restoredSnapshot.snapshot_revision, 18);

      const responsivePage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-responsive",
          initial_url: `http://127.0.0.1:${appAddress.port}/responsive`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(responsivePage.outcome.status, "succeeded");
      const desktopResponsiveSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        desktopResponsiveSnapshot.root.children.some(
          (node) => node.name === "Server desktop",
        ),
        "desktop viewport must keep the desktop user agent",
      );
      assert(
        desktopResponsiveSnapshot.root.children.some(
          (node) => node.name === "Viewport 900 / Media desktop / Touch 0 / Resize 0",
        ),
        "desktop viewport must expose desktop layout metrics",
      );
      const desktopResponsiveA = desktopResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive A",
      );
      const desktopResponsiveB = desktopResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive B",
      );
      assert(desktopResponsiveA?.bounds && desktopResponsiveB?.bounds);
      assert.equal(desktopResponsiveA.bounds.y, desktopResponsiveB.bounds.y);
      assert(
        desktopResponsiveB.bounds.x > desktopResponsiveA.bounds.x,
        "desktop responsive content must use the wide multi-column layout",
      );

      const mobileViewport = await client.call({
        type: "set_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 390,
            height: 844,
            surface_width: 390,
            surface_height: 844,
            device_scale_factor_millis: 1_000,
            device_type: "mobile",
          },
        },
      });
      assert.equal(mobileViewport.outcome.status, "succeeded");
      const mobileResponsiveSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        mobileResponsiveSnapshot.root.children.some(
          (node) => node.name === "Server desktop",
        ),
        "device emulation must resize the loaded document without reloading it",
      );
      assert(
        mobileResponsiveSnapshot.root.children.some(
          (node) => node.name === "Viewport 390 / Media mobile / Touch 5 / Resize 1",
        ),
        "one viewport command must produce one native resize with the requested device metrics",
      );
      const mobileResponsiveA = mobileResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive A",
      );
      const mobileResponsiveB = mobileResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive B",
      );
      assert(mobileResponsiveA?.bounds && mobileResponsiveB?.bounds);
      assert.equal(mobileResponsiveA.bounds.x, mobileResponsiveB.bounds.x);
      assert(
        mobileResponsiveB.bounds.y > mobileResponsiveA.bounds.y,
        "mobile responsive content must reflow vertically instead of being clipped",
      );
      const mobileScreenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-responsive",
          target: null,
          full_page: false,
          format: "png",
        },
      });
      assert.equal(mobileScreenshotResponse.outcome.status, "succeeded");
      assert.deepEqual(
        pngDimensions(await client.waitForBinary()),
        { width: 390, height: 844 },
        "CDP screenshot output must stay in logical viewport pixels at high DPI",
      );
      const clippedScreenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-responsive",
          target: null,
          clip: { x: 0.1, y: 0.25, width: 0.5, height: 0.25 },
          full_page: false,
          format: "png",
        },
      });
      assert.equal(clippedScreenshotResponse.outcome.status, "succeeded");
      assert.deepEqual(
        pngDimensions(await client.waitForBinary()),
        { width: 195, height: 211 },
        "annotation screenshots must contain only the selected viewport region",
      );

      const fittedWideViewport = await client.call({
        type: "set_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 1_280,
            height: 800,
            surface_width: 511,
            surface_height: 1_099,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
        },
      });
      assert.equal(fittedWideViewport.outcome.status, "succeeded");
      const fittedWideSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        fittedWideSnapshot.root.children.some(
          (node) => node.name === "Viewport 1280 / Media desktop / Touch 0 / Resize 2",
        ),
        "browser-native fitting must preserve the requested logical desktop viewport",
      );
      await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-responsive",
          format: "jpeg",
          quality: 88,
          max_width: 7_680,
          max_height: 4_320,
        },
      });
      const fittedWideCapture = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-responsive"
          && event.event.payload.width === 1_280
          && event.event.payload.surface_width === 511,
      );
      const fittedWideFrame = fittedWideCapture.event;
      assert.equal(fittedWideFrame.event.type, "screencast_frame");
      if (fittedWideFrame.event.type === "screencast_frame") {
        assert.equal(fittedWideFrame.event.payload.width, 1_280);
        assert.equal(fittedWideFrame.event.payload.height, 800);
        assert.equal(fittedWideFrame.event.payload.surface_width, 511);
        assert.equal(fittedWideFrame.event.payload.surface_height, 319);
      }
      assert.deepEqual(
        jpegDimensions(fittedWideCapture.binary),
        { width: 2_560, height: 1_600 },
        "Chromium must keep a native high-DPI frame while the panel surface fits it for display",
      );
      const resizedSurface = await client.call({
        type: "set_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 1_280,
            height: 800,
            surface_width: 400,
            surface_height: 900,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
        },
      });
      assert.equal(resizedSurface.outcome.status, "succeeded");
      const resizedSurfaceSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1_024 },
            subtree_ref: null,
          },
        }),
      );
      assert.equal(
        resizedSurfaceSnapshot.snapshot_revision,
        fittedWideSnapshot.snapshot_revision + 1,
        "surface-only fitting must not invalidate the canonical DOM snapshot",
      );
      assert(
        resizedSurfaceSnapshot.root.children.some(
          (node) => node.name === "Viewport 1280 / Media desktop / Touch 0 / Resize 2",
        ),
        "resizing only the Chromium output surface must not resize the logical page viewport",
      );
      const resizedSurfaceScroll = await client.call({
        type: "scroll",
        payload: {
          tab_id: "tab-responsive",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: null,
          delta_x: 0,
          delta_y: 100,
        },
      });
      assert.equal(resizedSurfaceScroll.outcome.status, "succeeded");
      const resizedSurfaceCapture = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-responsive"
          && event.event.payload.width === 1_280
          && event.event.payload.surface_width === 400,
      );
      const resizedSurfaceFrame = resizedSurfaceCapture.event;
      assert.equal(resizedSurfaceFrame.event.type, "screencast_frame");
      if (resizedSurfaceFrame.event.type === "screencast_frame") {
        assert.equal(resizedSurfaceFrame.event.payload.width, 1_280);
        assert.equal(resizedSurfaceFrame.event.payload.height, 800);
        assert.equal(resizedSurfaceFrame.event.payload.surface_width, 400);
        assert.equal(resizedSurfaceFrame.event.payload.surface_height, 250);
      }
      assert.deepEqual(
        jpegDimensions(resizedSurfaceCapture.binary),
        { width: 2_560, height: 1_600 },
        "surface-only changes must not lower the stable screencast resolution",
      );
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-responsive" },
      });

      const logicalOnlyViewport = await client.call({
        type: "set_logical_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 1_600,
            height: 900,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
        },
      });
      assert.equal(logicalOnlyViewport.outcome.status, "succeeded");
      await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-responsive",
          format: "jpeg",
          quality: 88,
          max_width: 7_680,
          max_height: 4_320,
        },
      });
      const logicalOnlyCapture = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-responsive"
          && event.event.payload.width === 1_600,
      );
      const logicalOnlyFrame = logicalOnlyCapture.event;
      assert.equal(logicalOnlyFrame.event.type, "screencast_frame");
      if (logicalOnlyFrame.event.type === "screencast_frame") {
        assert.equal(logicalOnlyFrame.event.payload.width, 1_600);
        assert.equal(logicalOnlyFrame.event.payload.height, 900);
        assert.equal(logicalOnlyFrame.event.payload.surface_width, 400);
        assert.equal(logicalOnlyFrame.event.payload.surface_height, 225);
      }
      assert.deepEqual(
        jpegDimensions(logicalOnlyCapture.binary),
        { width: 3_200, height: 1_800 },
        "logical viewport changes must preserve the existing display surface",
      );
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-responsive" },
      });

      await client.call({
        type: "update_control",
        payload: { fence: 1, mode: "agent" },
      });

      await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-a",
          initial_url: "about:blank",
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      await client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          navigation: {
            action: "url",
            url: `http://127.0.0.1:${appAddress.port}/`,
          },
        },
      });
      const firstSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      const button = firstSnapshot.root.children.find(
        (node) => node.name === "Increment",
      );
      const input = firstSnapshot.root.children.find(
        (node) => node.name === "Name",
      );
      const deleteAccount = firstSnapshot.root.children.find(
        (node) => node.name === "Delete account",
      );
      const showDialog = firstSnapshot.root.children.find(
        (node) => node.name === "Show dialog",
      );
      const openPopup = firstSnapshot.root.children.find(
        (node) => node.name === "Open popup",
      );
      const downloadSample = firstSnapshot.root.children.find(
        (node) => node.name === "Download sample",
      );
      const uploadFile = firstSnapshot.root.children.find(
        (node) => node.name === "Upload file",
      );
      const password = firstSnapshot.root.children.find(
        (node) => node.name === "Password",
      );
      const verificationCode = firstSnapshot.root.children.find(
        (node) => node.name === "Verification code",
      );
      const cardNumber = firstSnapshot.root.children.find(
        (node) => node.name === "Card number",
      );
      assert(button, "button snapshot ref should exist");
      assert(input, "input snapshot ref should exist");
      assert(deleteAccount, "sensitive action snapshot ref should exist");
      assert(showDialog, "dialog test ref should exist");
      assert(openPopup, "popup test ref should exist");
      assert(downloadSample, "download test ref should exist");
      assert(uploadFile, "file chooser test ref should exist");
      assert(password, "password snapshot ref should exist");
      assert(verificationCode, "verification code snapshot ref should exist");
      assert(cardNumber, "card number snapshot ref should exist");
      assert.equal(password.editable, false);
      assert.equal(password.value, null);
      assert.equal(password.sensitive_input_kind, "password");
      assert.equal(verificationCode.editable, false);
      assert.equal(verificationCode.value, null);
      assert.equal(verificationCode.sensitive_input_kind, "one_time_code");
      assert.equal(cardNumber.editable, false);
      assert.equal(cardNumber.value, null);
      assert.equal(cardNumber.sensitive_input_kind, "payment_card");
      const clickTestTarget = async (target: typeof showDialog) => {
        assert(target);
        const response = await client!.call({
          type: "click",
          payload: {
            tab_id: "tab-a",
            control: { mode: "agent", lease_id: "lease-a", fence: 1 },
            target: {
              snapshot_revision: firstSnapshot.snapshot_revision,
              element_ref: target.element_ref,
            },
          },
        });
        assert.equal(response.outcome.status, "succeeded");
      };
      await clickTestTarget(showDialog);
      const dialogEvent = await client.waitForEvent("dialog");
      assert.equal(dialogEvent.event.type, "dialog");
      if (dialogEvent.event.type === "dialog") {
        assert.equal(dialogEvent.event.payload.message, "Dialog content");
      }
      await clickTestTarget(openPopup);
      await client.waitForEvent("popup_blocked");
      await clickTestTarget(uploadFile);
      await client.waitForEvent("file_chooser");
      await clickTestTarget(downloadSample);
      const completedDownload = await client.waitForEvent("download");
      assert.equal(completedDownload.event.type, "download");
      const finalDownload = completedDownload.event.type === "download"
        && completedDownload.event.payload.state === "completed"
        ? completedDownload
        : await client.waitForEvent("download");
      assert.equal(finalDownload.event.type, "download");
      if (finalDownload.event.type === "download") {
        assert.equal(finalDownload.event.payload.state, "completed");
        assert.equal(finalDownload.event.payload.byte_length, 21);
      }
      const downloadedFiles = await readdir(downloadPath);
      assert.equal(downloadedFiles.length, 1);
      assert.equal(
        await readFile(join(downloadPath, downloadedFiles[0]), "utf8"),
        "Magi browser download",
      );
      const blockedSensitiveClick = await client.call({
        type: "click",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: firstSnapshot.snapshot_revision,
            element_ref: deleteAccount.element_ref,
          },
        },
      });
      assert.equal(blockedSensitiveClick.outcome.status, "failed");
      if (blockedSensitiveClick.outcome.status === "failed") {
        assert.equal(
          blockedSensitiveClick.outcome.payload.code,
          "browser_sensitive_action_requires_user",
        );
        assert.equal(blockedSensitiveClick.outcome.payload.side_effect_started, false);
      }
      const clickResponse = await client.call({
        type: "click",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: firstSnapshot.snapshot_revision,
            element_ref: button.element_ref,
          },
        },
      });
      assert.equal(clickResponse.outcome.status, "succeeded");
      if (clickResponse.outcome.status === "succeeded") {
        assert.equal(clickResponse.outcome.payload.type, "page_state");
      }
      const afterClick = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        afterClick.root.children.some((node) => node.name === "Count 1"),
        "click should update the page",
      );
      const currentInput = afterClick.root.children.find(
        (node) => node.name === "Name",
      );
      const redirectLink = afterClick.root.children.find(
        (node) => node.name === "Redirect",
      );
      assert(currentInput);
      assert(redirectLink);
      const typeResponse = await client.call({
        type: "type",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: afterClick.snapshot_revision,
            element_ref: currentInput.element_ref,
          },
          text: "Magi",
          replace: true,
        },
      });
      assert.equal(typeResponse.outcome.status, "succeeded");
      if (typeResponse.outcome.status === "succeeded") {
        assert.equal(typeResponse.outcome.payload.type, "page_state");
      }
      const afterTypeSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      const currentPassword = afterTypeSnapshot.root.children.find(
        (node) => node.name === "Password",
      );
      const currentRedirectLink = afterTypeSnapshot.root.children.find(
        (node) => node.name === "Redirect",
      );
      assert(currentPassword, "password ref should remain available after typing");
      assert(currentRedirectLink, "redirect ref should remain available after typing");
      const blockedSensitiveType = await client.call({
        type: "type",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: afterTypeSnapshot.snapshot_revision,
            element_ref: currentPassword.element_ref,
          },
          text: "should-not-be-sent",
          replace: true,
        },
      });
      assert.equal(blockedSensitiveType.outcome.status, "failed");
      if (blockedSensitiveType.outcome.status === "failed") {
        assert.equal(
          blockedSensitiveType.outcome.payload.code,
          "browser_sensitive_input_blocked",
        );
        assert.equal(blockedSensitiveType.outcome.payload.side_effect_started, false);
      }
      const redirectClick = await client.call({
        type: "click",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: afterTypeSnapshot.snapshot_revision,
            element_ref: currentRedirectLink.element_ref,
          },
        },
      });
      assert.equal(redirectClick.outcome.status, "succeeded");
      const screenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-a",
          target: null,
          full_page: false,
          format: "png",
        },
      });
      assert.equal(screenshotResponse.outcome.status, "succeeded");
      const screenshot = await client.waitForBinary();
      assert.deepEqual(pngDimensions(screenshot), { width: 900, height: 700 });

      const crossOriginRedirect = await client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          navigation: {
            action: "url",
            url: `http://127.0.0.1:${appAddress.port}/redirect`,
          },
        },
      });
      assert.equal(crossOriginRedirect.outcome.status, "succeeded");

      const blockedMetadataNavigation = await client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          navigation: {
            action: "url",
            url: "http://169.254.169.254/latest/meta-data",
          },
        },
      });
      assert.equal(blockedMetadataNavigation.outcome.status, "failed");
      if (blockedMetadataNavigation.outcome.status === "failed") {
        assert.equal(
          blockedMetadataNavigation.outcome.payload.code,
          "browser_navigation_target_blocked",
        );
        assert.equal(blockedMetadataNavigation.outcome.payload.side_effect_started, false);
      }

      await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-a",
          format: "jpeg",
          quality: 70,
          max_width: 1_800,
          max_height: 1_400,
        },
      });
      const screencastFrame = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-a",
      );
      const screencastEvent = screencastFrame.event;
      assert.equal(screencastEvent.event.type, "screencast_frame");
      assert.equal(screencastEvent.event.payload.width, 900);
      assert.equal(screencastEvent.event.payload.height, 700);
      assert.equal(screencastEvent.event.payload.surface_width, 900);
      assert.equal(screencastEvent.event.payload.surface_height, 700);
      assert.equal(screencastEvent.event.payload.device_scale_factor_millis, 2_000);
      const frame = screencastFrame.binary;
      assert.equal(frame[0], 0xff);
      assert.equal(frame[1], 0xd8);
      assert.deepEqual(jpegDimensions(frame), { width: 1_800, height: 1_400 });
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-a" },
      });

      await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-clipboard",
          initial_url: `http://127.0.0.1:${appAddress.port}/`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      const clipboardTargetSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-clipboard",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      const clipboardInput = clipboardTargetSnapshot.root.children.find(
        (node) => node.name === "Name",
      );
      assert(clipboardInput?.bounds, "clipboard test input should remain visible");
      const clipboardPoint = {
        x: clipboardInput.bounds.x + clipboardInput.bounds.width / 2,
        y: clipboardInput.bounds.y + clipboardInput.bounds.height / 2,
      };
      await client.call({
        type: "update_control",
        payload: { fence: 2, mode: "user" },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "mouse_down",
            ...clipboardPoint,
            button: "left",
            click_count: 1,
          },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "mouse_up",
            ...clipboardPoint,
            button: "left",
            click_count: 1,
          },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: { type: "insert_text", text: "Magi" },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_down",
            key: "a",
            code: "KeyA",
            key_code: 65,
            modifiers: 4,
          },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_up",
            key: "a",
            code: "KeyA",
            key_code: 65,
            modifiers: 4,
          },
        },
      });
      const copyResponse = await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_down",
            key: "c",
            code: "KeyC",
            key_code: 67,
            modifiers: 4,
          },
        },
      });
      assert.equal(copyResponse.outcome.status, "succeeded");
      if (copyResponse.outcome.status === "succeeded") {
        assert.equal(copyResponse.outcome.payload.type, "clipboard_text");
        if (copyResponse.outcome.payload.type === "clipboard_text") {
          assert.equal(copyResponse.outcome.payload.payload.operation, "copy");
          assert.equal(copyResponse.outcome.payload.payload.text, "Magi");
        }
      }
      const cutResponse = await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_down",
            key: "x",
            code: "KeyX",
            key_code: 88,
            modifiers: 4,
          },
        },
      });
      assert.equal(cutResponse.outcome.status, "succeeded");
      if (cutResponse.outcome.status === "succeeded") {
        assert.equal(cutResponse.outcome.payload.type, "clipboard_text");
        if (cutResponse.outcome.payload.type === "clipboard_text") {
          assert.equal(cutResponse.outcome.payload.payload.operation, "cut");
          assert.equal(cutResponse.outcome.payload.payload.text, "Magi");
        }
      }
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: { type: "insert_text", text: "Pasted" },
        },
      });
      const afterClipboardSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-clipboard",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        afterClipboardSnapshot.root.children.some(
          (node) => node.name === "Name" && node.value === "Pasted",
        ),
        "cut and paste should update the focused field",
      );
      const fenced = await client.call({
        type: "press",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          key: "Enter",
        },
      });
      assert.equal(fenced.outcome.status, "failed");
      if (fenced.outcome.status === "failed") {
        assert.equal(fenced.outcome.payload.code, "browser_lease_fenced");
      }

      const scaledPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-scaled-mobile",
          initial_url: `http://127.0.0.1:${appAddress.port}/`,
          viewport: {
            width: 600,
            height: 800,
            surface_width: 600,
            surface_height: 800,
            device_scale_factor_millis: 1_000,
            device_type: "mobile",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(scaledPage.outcome.status, "succeeded");
      const scaledScreenshot = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-scaled-mobile",
          target: null,
          full_page: false,
          format: "png",
        },
      });
      assert.equal(scaledScreenshot.outcome.status, "succeeded");
      assert.deepEqual(
        pngDimensions(await client.waitForBinary()),
        { width: 600, height: 800 },
      );
      const startScaledScreencast = await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-scaled-mobile",
          format: "jpeg",
          quality: 88,
          max_width: 7_680,
          max_height: 4_320,
        },
      });
      assert.equal(startScaledScreencast.outcome.status, "succeeded");
      const scaledFrame = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-scaled-mobile",
      );
      const scaledFrameEvent = scaledFrame.event;
      assert.equal(scaledFrameEvent.event.type, "screencast_frame");
      if (scaledFrameEvent.event.type === "screencast_frame") {
        assert.equal(scaledFrameEvent.event.payload.width, 980);
        assert.equal(scaledFrameEvent.event.payload.height, 1_307);
        assert.equal(scaledFrameEvent.event.payload.surface_width, 600);
        assert.equal(scaledFrameEvent.event.payload.surface_height, 800);
        assert.equal(
          scaledFrameEvent.event.payload.device_scale_factor_millis,
          1_224,
        );
      }
      assert.deepEqual(
        jpegDimensions(scaledFrame.binary),
        { width: 1_200, height: 1_600 },
      );
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-scaled-mobile" },
      });
    } finally {
      client?.close();
      await hostServer?.close();
      await new Promise<void>((accept) => appServer.close(() => accept()));
      await rm(profilePath, { recursive: true, force: true });
    }
  },
);

async function waitForScreencastFrame(
  client: HostTestClient,
  matches: (event: EventEnvelope) => boolean,
): Promise<{ event: EventEnvelope; binary: Buffer }> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const event = await client.waitForEvent("screencast_frame");
    const binary = await client.waitForBinary();
    if (matches(event)) return { event, binary };
  }
  throw new Error("Host did not produce the expected screencast frame");
}

function snapshotFrom(response: ResponseEnvelope): HostSnapshot {
  assert.equal(
    response.outcome.status,
    "succeeded",
    JSON.stringify(response.outcome),
  );
  if (response.outcome.status !== "succeeded") {
    throw new Error("snapshot command failed");
  }
  assert.equal(response.outcome.payload.type, "snapshot");
  if (response.outcome.payload.type !== "snapshot") {
    throw new Error("snapshot response has the wrong type");
  }
  return response.outcome.payload.payload;
}

class HostTestClient {
  readonly #websocket: WebSocket;
  readonly #responses = new Map<string, (response: ResponseEnvelope) => void>();
  readonly #events: EventEnvelope[] = [];
  readonly #eventWaiters = new Map<
    HostEventType,
    Array<(event: EventEnvelope) => void>
  >();
  readonly #binary: Buffer[] = [];
  readonly #binaryWaiters: Array<(payload: Buffer) => void> = [];
  #sequence = 0;

  constructor(url: string, token: string) {
    this.#websocket = new WebSocket(url, {
      headers: { authorization: `Bearer ${token}` },
    });
    this.#websocket.on("message", (data, isBinary) => {
      if (isBinary) {
        const payload = Buffer.from(data as Buffer);
        const waiter = this.#binaryWaiters.shift();
        if (waiter) waiter(payload);
        else this.#binary.push(payload);
        return;
      }
      const value = JSON.parse(data.toString()) as
        | ResponseEnvelope
        | EventEnvelope;
      if ("request_id" in value) {
        this.#responses.get(value.request_id)?.(value);
        this.#responses.delete(value.request_id);
      } else {
        const waiters = this.#eventWaiters.get(value.event.type);
        const waiter = waiters?.shift();
        if (waiter) waiter(value);
        else this.#events.push(value);
      }
    });
  }

  open(): Promise<void> {
    return new Promise((accept, reject) => {
      if (this.#websocket.readyState === WebSocket.OPEN) {
        accept();
        return;
      }
      this.#websocket.once("open", () => accept());
      this.#websocket.once("error", reject);
    });
  }

  call(command: RequestEnvelope["command"]): Promise<ResponseEnvelope> {
    const requestId = `request-${++this.#sequence}`;
    const request: RequestEnvelope = {
      request_id: requestId,
      protocol_version: PROTOCOL_VERSION,
      command,
    };
    return new Promise((accept, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`Host response timed out: ${requestId}`)),
        10_000,
      );
      this.#responses.set(requestId, (response) => {
        clearTimeout(timer);
        accept(response);
      });
      this.#websocket.send(JSON.stringify(request));
    });
  }

  waitForEvent(type: HostEventType): Promise<EventEnvelope> {
    const existingIndex = this.#events.findIndex((event) => event.event.type === type);
    if (existingIndex >= 0) {
      return Promise.resolve(this.#events.splice(existingIndex, 1)[0]);
    }
    return new Promise((accept, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`Host event timed out: ${type}`)),
        10_000,
      );
      const waiters = this.#eventWaiters.get(type) ?? [];
      waiters.push((event) => {
        clearTimeout(timer);
        accept(event);
      });
      this.#eventWaiters.set(type, waiters);
    });
  }

  waitForBinary(): Promise<Buffer> {
    const existing = this.#binary.shift();
    if (existing) return Promise.resolve(existing);
    return new Promise((accept, reject) => {
      const timer = setTimeout(
        () => reject(new Error("Host binary payload timed out")),
        10_000,
      );
      this.#binaryWaiters.push((payload) => {
        clearTimeout(timer);
        accept(payload);
      });
    });
  }

  close(): void {
    this.#websocket.terminate();
  }
}

type HostEventType = EventEnvelope["event"]["type"];
