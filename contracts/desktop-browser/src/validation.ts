import { Ajv2020 } from "ajv/dist/2020.js";
import desktopControlSchema from "../desktop-control.schema.json" with { type: "json" };
import desktopIpcSchema from "../desktop-ipc.schema.json" with { type: "json" };
import workerIpcSchema from "../worker-ipc.schema.json" with { type: "json" };
import type {
  BrowserHostEventEnvelope,
  BrowserHostRequestEnvelope,
  BrowserHostResponseEnvelope,
  MainToWorkerMessage,
  WorkerToMainMessage,
} from "./index.js";

const ajv = new Ajv2020({
  allErrors: true,
  strictSchema: true,
  strictTypes: false,
  allowUnionTypes: true,
  validateFormats: false,
});

ajv.addSchema(desktopControlSchema);
// worker-ipc.schema.json references the desktop-control definitions by their
// stable schema id. Register both documents before compiling directional
// validators so validation never depends on import evaluation order.
ajv.addSchema(workerIpcSchema);

const validateDesktopControl = ajv.compile<BrowserHostRequestEnvelope>(desktopControlSchema);
const validateDesktopIpc = ajv.compile<{ channel: string; payload: unknown }>(desktopIpcSchema);
const validateMainToWorker = ajv.compile<MainToWorkerMessage>({
  $ref: `${workerIpcSchema.$id}#/$defs/mainToWorkerMessage`,
});
const validateWorkerToMain = ajv.compile<WorkerToMainMessage>({
  $ref: `${workerIpcSchema.$id}#/$defs/workerToMainMessage`,
});

export function parseBrowserHostRequest(value: unknown): BrowserHostRequestEnvelope {
  return parseValidated(validateDesktopControl, value, "browser_protocol_invalid") as BrowserHostRequestEnvelope;
}

export function parseBrowserHostResponse(value: unknown): BrowserHostResponseEnvelope {
  const validateResponse = ajv.getSchema(`${desktopControlSchema.$id}#/$defs/responseEnvelope`)
    ?? ajv.compile<BrowserHostResponseEnvelope>({
      $ref: `${desktopControlSchema.$id}#/$defs/responseEnvelope`,
    });
  return parseValidated(validateResponse, value, "browser_protocol_invalid") as BrowserHostResponseEnvelope;
}

export function parseBrowserHostEventEnvelope(value: unknown): BrowserHostEventEnvelope {
  const validateEvent = ajv.getSchema(`${desktopControlSchema.$id}#/$defs/eventEnvelope`)
    ?? ajv.compile<BrowserHostEventEnvelope>({
      $ref: `${desktopControlSchema.$id}#/$defs/eventEnvelope`,
    });
  return parseValidated(validateEvent, value, "browser_protocol_invalid") as BrowserHostEventEnvelope;
}

export function assertDesktopIpcMessage(channel: string, payload: unknown): void {
  parseValidated(validateDesktopIpc, { channel, payload }, "desktop_ipc_invalid");
}

export function parseMainToWorkerMessage(value: unknown): MainToWorkerMessage {
  return parseValidated(validateMainToWorker, value, "worker_ipc_invalid") as MainToWorkerMessage;
}

export function parseWorkerToMainMessage(value: unknown): WorkerToMainMessage {
  return parseValidated(validateWorkerToMain, value, "worker_ipc_invalid") as WorkerToMainMessage;
}

function parseValidated<T>(
  validator: ((value: unknown) => value is T) & { errors?: Array<{ instancePath?: string; message?: string }> | null },
  value: unknown,
  code: string,
): T {
  if (validator(value)) return value;
  const detail = (validator.errors ?? [])
    .slice(0, 3)
    .map((error) => `${error.instancePath || "/"} ${error.message ?? "invalid"}`)
    .join("; ");
  throw new Error(detail ? `${code}:${detail}` : code);
}
