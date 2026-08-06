import type { HostControl, HostControlMode } from "./protocol";

export class ProtocolFailure extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly recoverable: boolean,
    readonly sideEffectStarted: boolean,
    readonly diagnostic?: string,
  ) {
    super(message);
    this.name = "ProtocolFailure";
  }
}

export class ControlFence {
  #fence = 0;
  #mode: HostControlMode = "disabled";

  get fence(): number {
    return this.#fence;
  }

  get mode(): HostControlMode {
    return this.#mode;
  }

  update(fence: number, mode: HostControlMode): void {
    requireSafeInteger("fence", fence);
    if (fence < this.#fence) {
      throw new ProtocolFailure(
        "browser_fence_regression",
        `control fence cannot move backwards: current=${this.#fence}, received=${fence}`,
        false,
        false,
      );
    }
    this.#fence = fence;
    this.#mode = mode;
  }

  validate(control: HostControl): void {
    requireSafeInteger("control.fence", control.fence);
    if (control.fence !== this.#fence) {
      throw new ProtocolFailure(
        "browser_lease_fenced",
        `browser control fence is stale: current=${this.#fence}, received=${control.fence}`,
        true,
        false,
      );
    }
    if (control.mode !== this.#mode) {
      throw new ProtocolFailure(
        "browser_control_mode_mismatch",
        `browser is controlled by ${this.#mode}, not ${control.mode}`,
        true,
        false,
      );
    }
    if (control.mode === "agent" && control.lease_id.trim().length === 0) {
      throw new ProtocolFailure(
        "browser_lease_invalid",
        "agent control requires a lease id",
        false,
        false,
      );
    }
  }
}

export function requireSafeInteger(name: string, value: number): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new ProtocolFailure(
      "browser_protocol_invalid",
      `${name} must be a non-negative safe integer`,
      false,
      false,
    );
  }
}
