import { DESKTOP_BROWSER_PROTOCOL_VERSION, type MainToWorkerMessage, type WorkerReadyMessage } from "@magi/desktop-browser-contracts";
import { CdpClient, parentPort } from "./cdp-client.js";
import { BrowserAutomationRuntime } from "./runtime.js";

const port = parentPort();
const cdp = new CdpClient(port);
const runtime = new BrowserAutomationRuntime(cdp);

const ready: WorkerReadyMessage = {
  type: "worker_ready",
  worker_epoch: process.env.MAGI_BROWSER_WORKER_EPOCH ?? "",
  protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
};
port.postMessage(ready);

port.on("message", (event) => {
  const message: MainToWorkerMessage = event.data;
  if (message.type !== "worker_command") return;
  void runtime.execute(message.call_id, message.binding, message.command)
    .then((result) => port.postMessage(result));
});

process.once("exit", () => cdp.close());
