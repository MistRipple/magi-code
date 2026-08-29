import {
  DESKTOP_BROWSER_PROTOCOL_VERSION,
  type BrowserCommandError,
  type BrowserSurfaceBinding,
  type MainToWorkerMessage,
  type WorkerCommandResponse,
  type WorkerRebindAck,
  type WorkerReadyMessage,
} from "@magi/desktop-browser-contracts";
import {
  parseMainToWorkerMessage,
  parseWorkerToMainMessage,
} from "@magi/desktop-browser-contracts/validation";
import { CdpClient, parentPort } from "./cdp-client.js";
import { BrowserAutomationRuntime } from "./runtime.js";
import { randomUUID } from "node:crypto";

const port = parentPort();
const cdp = new CdpClient(port);
const workerEpoch = process.env.MAGI_BROWSER_WORKER_EPOCH?.trim() || `worker-${randomUUID()}`;
const runtime = new BrowserAutomationRuntime(cdp, workerEpoch);

const ready: WorkerReadyMessage = {
  type: "worker_ready",
  worker_epoch: workerEpoch,
  protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
};
postWorkerMessage(ready);

port.on("message", (event) => {
  let message: MainToWorkerMessage;
  try {
    message = parseMainToWorkerMessage(event.data);
  } catch (cause) {
    logProtocolError("invalid inbound message", cause);
    return;
  }
  if (message.type === "worker_rebind") {
    runtime.rebind(message.bindings);
    const ack: WorkerRebindAck = {
      type: "worker_rebind_ack",
      worker_epoch: workerEpoch,
      rebind_id: message.rebind_id,
      binding_count: message.bindings.length,
    };
    postWorkerMessage(ack);
    return;
  }
  if (message.type === "worker_cancel") {
    runtime.cancel(message.call_id);
    return;
  }
  if (message.type !== "worker_command") return;
  void runtime.execute(message.call_id, message.binding, message.command)
    .then((result) => postWorkerMessage(result))
    .catch((cause) => postWorkerMessage(unhandledWorkerResult(message.call_id, message.binding, cause)));
});

process.once("exit", () => cdp.close());

function postWorkerMessage(message: WorkerReadyMessage | WorkerCommandResponse | WorkerRebindAck): void {
  try {
    port.postMessage(parseWorkerToMainMessage(message));
  } catch (cause) {
    logProtocolError("invalid outbound message", cause);
  }
}

function unhandledWorkerResult(
  callId: string,
  binding: BrowserSurfaceBinding,
  cause: unknown,
): WorkerCommandResponse {
  const error: BrowserCommandError = {
    code: "browser_worker_unhandled_failure",
    message: cause instanceof Error ? cause.message : String(cause),
    recoverable: true,
    side_effect_started: true,
    diagnostic: cause instanceof Error ? cause.stack ?? null : null,
  };
  return {
    type: "worker_result",
    call_id: callId,
    binding,
    outcome: { status: "indeterminate", payload: error },
  };
}

function logProtocolError(scope: string, cause: unknown): void {
  console.error(`[browser-worker] ${scope}`, cause instanceof Error ? cause.message : String(cause));
}
