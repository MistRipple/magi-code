import type { Locator, Page } from "playwright-core";
import { ProtocolFailure } from "./control";
import type {
  HostSnapshot,
  Rect,
  SnapshotLimits,
  SnapshotNode,
  SnapshotTarget,
} from "./protocol";

interface RawSnapshotNode {
  selector: string;
  tagName: string;
  role: string | null;
  name: string | null;
  value: string | null;
  description: string | null;
  disabled: boolean;
  focused: boolean;
  editable: boolean;
  sensitiveInputKind: "password" | "one_time_code" | "payment_card" | null;
  visible: boolean;
  bounds: Rect;
  fingerprint: string;
  childElementCount: number;
}

interface SnapshotReference {
  selector: string;
  tagName: string;
  fingerprint: string;
}

export class SnapshotRegistry {
  #revision = 0;
  #references = new Map<string, SnapshotReference>();

  constructor(initialRevision = 0) {
    this.#revision = Math.max(0, Math.floor(initialRevision));
  }

  get revision(): number {
    return this.#revision;
  }

  invalidate(): void {
    this.#revision += 1;
    this.#references.clear();
  }

  async capture(
    page: Page,
    tabId: string,
    requestedLimits: SnapshotLimits,
    subtreeRef?: string | null,
  ): Promise<HostSnapshot> {
    const limits = normalizeLimits(requestedLimits);
    let root: Locator = page.locator("body");
    if (subtreeRef) {
      root = await this.resolveRef(page, this.#revision, subtreeRef);
    }
    this.#revision += 1;
    this.#references.clear();
    const scanLimit = Math.min(Math.max(limits.max_nodes * 20, 1_000), 20_000);
    const raw = await root.evaluate(
      (rootElement, input) => {
        const all = [
          rootElement,
          ...Array.from(rootElement.querySelectorAll("*")),
        ].slice(0, input.scanLimit);
        const totalNodes = all.length;
        const output: RawSnapshotNode[] = [];
        let textBytes = 0;

        const redact = (value: string): string =>
          value
            .replace(/\b(sk-[A-Za-z0-9_-]{12,})\b/g, "[REDACTED]")
            .replace(/\bBearer\s+[A-Za-z0-9._~+\/-]+=*/gi, "[REDACTED]")
            .replace(
              /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/g,
              "[REDACTED]",
            );
        const text = (value: string | null | undefined, max: number) => {
          const normalized = redact((value ?? "").replace(/\s+/g, " ").trim());
          return normalized.length > max
            ? `${normalized.slice(0, max - 1)}…`
            : normalized;
        };
        const roleFor = (element: Element): string | null => {
          const explicit = element.getAttribute("role");
          if (explicit) return explicit;
          const tag = element.tagName.toLowerCase();
          if (tag === "a" && element.hasAttribute("href")) return "link";
          if (tag === "button") return "button";
          if (tag === "textarea") return "textbox";
          if (tag === "select") return "combobox";
          if (tag === "img") return "img";
          if (tag === "input") {
            const type = (element.getAttribute("type") ?? "text").toLowerCase();
            if (type === "checkbox") return "checkbox";
            if (type === "radio") return "radio";
            if (type === "button" || type === "submit") return "button";
            return "textbox";
          }
          return null;
        };
        const sensitiveInputKind = (
          element: Element,
        ): "password" | "one_time_code" | "payment_card" | null => {
          const input = element as HTMLInputElement;
          const type = (input.type || element.getAttribute("type") || "text").toLowerCase();
          const autocomplete = (element.getAttribute("autocomplete") ?? "").toLowerCase();
          const descriptor = [
            element.getAttribute("name"),
            element.getAttribute("id"),
            element.getAttribute("aria-label"),
            element.getAttribute("placeholder"),
            autocomplete,
          ]
            .filter(Boolean)
            .join(" ")
            .toLowerCase();
          if (type === "password" || /password|passcode|passwd/.test(descriptor)) {
            return "password";
          }
          if (
            autocomplete === "one-time-code" ||
            /one[- ]?time|otp|verification code|security code|auth code/.test(descriptor)
          ) {
            return "one_time_code";
          }
          if (
            autocomplete.startsWith("cc-") ||
            /credit[- ]?card|card number|cardholder|cvv|cvc|ccv|expiration date|expiry date/.test(descriptor)
          ) {
            return "payment_card";
          }
          return null;
        };
        const cssPath = (element: Element): string => {
          const escaped = (value: string) =>
            globalThis.CSS?.escape
              ? globalThis.CSS.escape(value)
              : value.replace(/[^A-Za-z0-9_-]/g, "\\$&");
          const id = element.getAttribute("id");
          if (id) return `#${escaped(id)}`;
          const testId = element.getAttribute("data-testid");
          if (testId) return `[data-testid="${escaped(testId)}"]`;
          const parts: string[] = [];
          let current: Element | null = element;
          while (current && current.tagName.toLowerCase() !== "html") {
            const tag = current.tagName.toLowerCase();
            const siblings = current.parentElement
              ? Array.from(current.parentElement.children).filter(
                  (candidate) => candidate.tagName === current?.tagName,
                )
              : [];
            const index = siblings.indexOf(current) + 1;
            parts.unshift(`${tag}:nth-of-type(${Math.max(index, 1)})`);
            current = current.parentElement;
          }
          return `html > ${parts.join(" > ")}`;
        };

        for (const element of all) {
          const html = element as HTMLElement;
          const style = getComputedStyle(element);
          const rect = element.getBoundingClientRect();
          const visible =
            style.display !== "none" &&
            style.visibility !== "hidden" &&
            Number(style.opacity) !== 0 &&
            rect.width > 0 &&
            rect.height > 0;
          if (!visible) continue;
          const role = roleFor(element);
          const tagName = element.tagName.toLowerCase();
          const sensitiveKind = sensitiveInputKind(element);
          const editable =
            html.isContentEditable ||
            tagName === "textarea" ||
            (tagName === "input" &&
              (element.getAttribute("type") ?? "text").toLowerCase() !== "password") &&
            sensitiveKind === null;
          const directText = text(
            Array.from(element.childNodes)
              .filter((node) => node.nodeType === Node.TEXT_NODE)
              .map((node) => node.textContent ?? "")
              .join(" "),
            240,
          );
          if (!role && !editable && !directText) continue;
          const inputElement = element as HTMLInputElement;
          const inputType = (inputElement.type ?? "").toLowerCase();
          const name = text(
            element.getAttribute("aria-label") ??
              element.getAttribute("alt") ??
              element.getAttribute("title") ??
              directText,
            240,
          );
          const value =
            sensitiveKind !== null
              ? null
              : text(
                  editable
                    ? inputElement.value || element.getAttribute("value")
                    : element.getAttribute("aria-valuetext"),
                  240,
                ) || null;
          const description =
            text(element.getAttribute("aria-description"), 240) || null;
          const fingerprint = [
            tagName,
            element.getAttribute("id") ?? "",
            element.getAttribute("data-testid") ?? "",
            role ?? "",
            name,
            directText.slice(0, 80),
          ].join("|");
          const candidateTextBytes = new TextEncoder().encode(
            `${name}${value ?? ""}${description ?? ""}`,
          ).length;
          if (
            output.length >= input.maxNodes ||
            textBytes + candidateTextBytes > input.maxTextBytes
          ) {
            break;
          }
          textBytes += candidateTextBytes;
          output.push({
            selector: cssPath(element),
            tagName,
            role,
            name: name || null,
            value,
            description,
            disabled:
              element.hasAttribute("disabled") ||
              element.getAttribute("aria-disabled") === "true",
            focused: document.activeElement === element,
            editable,
            sensitiveInputKind: sensitiveKind,
            visible,
            bounds: {
              x: rect.x,
              y: rect.y,
              width: rect.width,
              height: rect.height,
            },
            fingerprint,
            childElementCount: element.childElementCount,
          });
        }
        return { output, totalNodes, textBytes };
      },
      {
        maxNodes: limits.max_nodes,
        maxTextBytes: limits.max_text_bytes,
        scanLimit,
      },
    );

    const children: SnapshotNode[] = raw.output.map((node, index) => {
      const elementRef = `e-${this.#revision}-${index + 1}`;
      this.#references.set(elementRef, {
        selector: node.selector,
        tagName: node.tagName,
        fingerprint: node.fingerprint,
      });
      return {
        element_ref: elementRef,
        role: node.role,
        name: node.name,
        value: node.value,
        description: node.description,
        disabled: node.disabled,
        focused: node.focused,
        editable: node.editable,
        sensitive_input_kind: node.sensitiveInputKind,
        visible: node.visible,
        bounds: node.bounds,
        children: [],
      };
    });
    const continuationRefs = raw.output
      .map((node, index) => ({ node, ref: `e-${this.#revision}-${index + 1}` }))
      .filter(({ node }) => node.childElementCount > 0)
      .slice(-8)
      .map(({ ref }) => ref);
    return {
      tab_id: tabId,
      snapshot_revision: this.#revision,
      root: {
        element_ref: "root",
        role: "document",
        name: (await page.title()) || null,
        value: null,
        description: null,
        disabled: false,
        focused: false,
        editable: false,
        sensitive_input_kind: null,
        visible: true,
        bounds: null,
        children,
      },
      returned_nodes: children.length,
      total_nodes: raw.totalNodes,
      text_bytes: raw.textBytes,
      truncated: children.length < raw.totalNodes,
      continuation_refs: continuationRefs,
    };
  }

  async resolve(page: Page, target: SnapshotTarget): Promise<Locator> {
    return this.resolveRef(page, target.snapshot_revision, target.element_ref);
  }

  private async resolveRef(
    page: Page,
    revision: number,
    elementRef: string,
  ): Promise<Locator> {
    if (revision !== this.#revision) {
      throw new ProtocolFailure(
        "browser_snapshot_stale",
        `snapshot revision changed: current=${this.#revision}, received=${revision}`,
        true,
        false,
      );
    }
    const reference = this.#references.get(elementRef);
    if (!reference) {
      throw new ProtocolFailure(
        "browser_snapshot_ref_unknown",
        `snapshot element ref does not exist: ${elementRef}`,
        true,
        false,
      );
    }
    const locator = page.locator(reference.selector).first();
    if ((await locator.count()) !== 1) {
      throw staleElement(elementRef);
    }
    const fingerprint = await locator.evaluate((element) => {
      const text = (value: string | null | undefined, max: number) => {
        const normalized = (value ?? "").replace(/\s+/g, " ").trim();
        return normalized.length > max ? normalized.slice(0, max) : normalized;
      };
      const tag = element.tagName.toLowerCase();
      const role =
        element.getAttribute("role") ??
        (tag === "button"
          ? "button"
          : tag === "a" && element.hasAttribute("href")
            ? "link"
            : tag === "input" || tag === "textarea"
              ? "textbox"
              : "");
      const directText = text(
        Array.from(element.childNodes)
          .filter((node) => node.nodeType === Node.TEXT_NODE)
          .map((node) => node.textContent ?? "")
          .join(" "),
        240,
      );
      const name = text(
        element.getAttribute("aria-label") ??
          element.getAttribute("alt") ??
          element.getAttribute("title") ??
          directText,
        240,
      );
      return [
        tag,
        element.getAttribute("id") ?? "",
        element.getAttribute("data-testid") ?? "",
        role,
        name,
        directText.slice(0, 80),
      ].join("|");
    });
    if (fingerprint !== reference.fingerprint) {
      throw staleElement(elementRef);
    }
    return locator;
  }
}

function normalizeLimits(limits: SnapshotLimits): SnapshotLimits {
  const maxNodes = Math.max(1, Math.min(Math.floor(limits.max_nodes), 400));
  const maxTextBytes = Math.max(
    1_024,
    Math.min(Math.floor(limits.max_text_bytes), 32 * 1024),
  );
  return { max_nodes: maxNodes, max_text_bytes: maxTextBytes };
}

function staleElement(elementRef: string): ProtocolFailure {
  return new ProtocolFailure(
    "browser_snapshot_stale",
    `snapshot element changed or disappeared: ${elementRef}`,
    true,
    false,
  );
}
