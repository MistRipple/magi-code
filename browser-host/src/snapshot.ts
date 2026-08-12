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
  documentOrder: number;
  priority: number;
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
  #referencesByRevision = new Map<number, Map<string, SnapshotReference>>();

  constructor(initialRevision = 0) {
    this.#revision = Math.max(0, Math.floor(initialRevision));
  }

  get revision(): number {
    return this.#revision;
  }

  advanceTo(revision: number): void {
    const nextRevision = Math.max(0, Math.floor(revision));
    if (nextRevision <= this.#revision) return;
    this.#revision = nextRevision;
    this.#referencesByRevision.clear();
  }

  invalidate(): void {
    this.#revision += 1;
    this.#referencesByRevision.clear();
  }

  async capture(
    page: Page,
    tabId: string,
    requestedLimits: SnapshotLimits,
    subtreeRef?: string | null,
  ): Promise<HostSnapshot> {
    const limits = normalizeLimits(requestedLimits);
    let root: Locator = page.locator("body");
    if (subtreeRef && subtreeRef !== "root") {
      root = await this.resolveRef(page, snapshotRevisionForRef(subtreeRef), subtreeRef);
    }
    this.#revision += 1;
    const references = new Map<string, SnapshotReference>();
    const scanLimit = Math.min(Math.max(limits.max_nodes * 20, 1_000), 20_000);
    const raw = await root.evaluate(
      (rootElement, input) => {
        const descendants = Array.from(rootElement.querySelectorAll("*"));
        const all = [rootElement, ...descendants].slice(0, input.scanLimit);
        let totalNodes = 0;
        let truncated = descendants.length + 1 > input.scanLimit;
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
          if (/^h[1-6]$/.test(tag)) return "heading";
          if (tag === "p") return "paragraph";
          if (tag === "input") {
            const type = (element.getAttribute("type") ?? "text").toLowerCase();
            if (type === "checkbox") return "checkbox";
            if (type === "radio") return "radio";
            if (type === "button" || type === "submit") return "button";
            if (type === "search") return "searchbox";
            const searchIdentity = [
              element.getAttribute("id"),
              element.getAttribute("name"),
              element.getAttribute("aria-label"),
            ]
              .filter(Boolean)
              .join(" ")
              .toLowerCase();
            const formAction =
              element.closest("form")?.getAttribute("action")?.toLowerCase() ?? "";
            if (
              /(^|[^a-z])(search|query|keyword|kw|wd|q)([^a-z]|$)/.test(searchIdentity) ||
              /(^|\/)search([/?#]|$)|(^|\/)s([/?#]|$)/.test(formAction)
            ) {
              return "searchbox";
            }
            return "textbox";
          }
          return null;
        };
        const semanticAncestorSelector = [
          "a[href]",
          "button",
          "input",
          "textarea",
          "select",
          "h1",
          "h2",
          "h3",
          "h4",
          "h5",
          "h6",
          "p",
          "[role]",
          "[contenteditable='true']",
        ].join(",");
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

        for (const [documentOrder, element] of all.entries()) {
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
          if (
            !role
            && !editable
            && element.parentElement?.closest(semanticAncestorSelector)
          ) {
            continue;
          }
          if (!role && !editable && !directText) continue;
          totalNodes += 1;
          const inViewport =
            rect.right > 0 &&
            rect.bottom > 0 &&
            rect.left < globalThis.innerWidth &&
            rect.top < globalThis.innerHeight;
          const semanticText =
            role &&
            [
              "button",
              "checkbox",
              "combobox",
              "heading",
              "link",
              "paragraph",
              "radio",
              "searchbox",
              "switch",
              "textbox",
            ].includes(role)
              ? text(html.innerText, role === "link" ? 120 : 240)
              : directText;
          const inputElement = element as HTMLInputElement;
          const labelledBy = (element.getAttribute("aria-labelledby") ?? "")
            .split(/\s+/)
            .filter(Boolean)
            .map((id) => document.getElementById(id)?.textContent ?? "")
            .join(" ");
          const controlLabels =
            element instanceof HTMLInputElement ||
            element instanceof HTMLTextAreaElement ||
            element instanceof HTMLSelectElement
              ? Array.from(element.labels ?? [])
                  .map((label) => label.textContent ?? "")
                  .join(" ")
              : "";
          const name = text(
            [
              element.getAttribute("aria-label"),
              labelledBy,
              controlLabels,
              element.getAttribute("alt"),
              element.getAttribute("title"),
              semanticText,
              role === "searchbox" ? "Search" : null,
              role || editable
                ? element.getAttribute("placeholder") ??
                  element.getAttribute("name") ??
                  element.getAttribute("id")
                : null,
            ].find((value) => value?.trim()) ?? "",
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
          const explicitDescription = text(
            element.getAttribute("aria-description"),
            240,
          );
          const description = explicitDescription
            || (semanticText && semanticText !== name ? semanticText : null);
          const fingerprint = [
            tagName,
            element.getAttribute("id") ?? "",
            element.getAttribute("data-testid") ?? "",
            element.getAttribute("name") ?? "",
            element.getAttribute("type") ?? "",
            element.getAttribute("href") ?? "",
            element.getAttribute("role") ?? "",
          ].join("|");
          output.push({
            documentOrder,
            priority: html.matches(":focus")
              ? 0
              : editable || ["searchbox", "textbox", "combobox"].includes(role ?? "")
                ? 1
                : ["button", "checkbox", "radio", "switch"].includes(role ?? "")
                  ? 2
                  : role === "link"
                    ? 3
                    : role === "heading"
                      ? 4
                      : role === "paragraph"
                        ? 5
                      : inViewport
                        ? 5
                        : 6,
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
        output.sort(
          (left, right) =>
            left.priority - right.priority ||
            left.documentOrder - right.documentOrder,
        );
        const selected: RawSnapshotNode[] = [];
        const priorityBudgets = [4, 24, 32, 48, 20, 28, 4];
        const selectedByPriority = new Set<number>();
        textBytes = 0;
        for (const priority of priorityBudgets.keys()) {
          const budget = priorityBudgets[priority];
          let selectedInPriority = 0;
          for (const node of output) {
            if (node.priority !== priority || selectedInPriority >= budget) continue;
            const candidateTextBytes = new TextEncoder().encode(
              `${node.name ?? ""}${node.value ?? ""}${node.description ?? ""}`,
            ).length;
            if (
              selected.length >= input.maxNodes ||
              textBytes + candidateTextBytes > input.maxTextBytes
            ) {
              truncated = true;
              continue;
            }
            selected.push(node);
            selectedByPriority.add(node.documentOrder);
            selectedInPriority += 1;
            textBytes += candidateTextBytes;
          }
        }
        for (const node of output) {
          if (selectedByPriority.has(node.documentOrder)) continue;
          const candidateTextBytes = new TextEncoder().encode(
            `${node.name ?? ""}${node.value ?? ""}${node.description ?? ""}`,
          ).length;
          if (
            selected.length >= input.maxNodes ||
            textBytes + candidateTextBytes > input.maxTextBytes
          ) {
            truncated = true;
            continue;
          }
          selected.push(node);
          textBytes += candidateTextBytes;
        }
        selected.sort((left, right) => left.documentOrder - right.documentOrder);
        return { output: selected, totalNodes, textBytes, truncated };
      },
      {
        maxNodes: limits.max_nodes,
        maxTextBytes: limits.max_text_bytes,
        scanLimit,
      },
    );

    const children: SnapshotNode[] = raw.output.map((node, index) => {
      const elementRef = `e-${this.#revision}-${index + 1}`;
      references.set(elementRef, {
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
      .map((node, index) => ({
        node,
        ref: `e-${this.#revision}-${index + 1}`,
      }))
      .filter(({ node }) => node.childElementCount > 0)
      .slice(-8)
      .map(({ ref }) => ref);
    const snapshot = {
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
      truncated: raw.truncated,
      continuation_refs: continuationRefs,
    };
    this.#referencesByRevision.set(this.#revision, references);
    while (this.#referencesByRevision.size > 8) {
      const oldestRevision = this.#referencesByRevision.keys().next().value;
      if (oldestRevision === undefined) break;
      this.#referencesByRevision.delete(oldestRevision);
    }
    return snapshot;
  }

  async resolve(page: Page, target: SnapshotTarget): Promise<Locator> {
    return this.resolveRef(page, target.snapshot_revision, target.element_ref);
  }

  private async resolveRef(
    page: Page,
    revision: number,
    elementRef: string,
  ): Promise<Locator> {
    const references = this.#referencesByRevision.get(revision);
    if (!references) {
      throw new ProtocolFailure(
        "browser_snapshot_stale",
        `snapshot revision changed: current=${this.#revision}, received=${revision}`,
        true,
        false,
      );
    }
    const reference = references.get(elementRef);
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
      const tag = element.tagName.toLowerCase();
      return [
        tag,
        element.getAttribute("id") ?? "",
        element.getAttribute("data-testid") ?? "",
        element.getAttribute("name") ?? "",
        element.getAttribute("type") ?? "",
        element.getAttribute("href") ?? "",
        element.getAttribute("role") ?? "",
      ].join("|");
    });
    if (fingerprint !== reference.fingerprint) {
      throw staleElement(elementRef);
    }
    return locator;
  }
}

function snapshotRevisionForRef(elementRef: string): number {
  const match = /^e-(\d+)-\d+$/.exec(elementRef);
  if (!match) {
    throw new ProtocolFailure(
      "browser_snapshot_ref_unknown",
      `snapshot element ref does not exist: ${elementRef}`,
      true,
      false,
    );
  }
  return Number(match[1]);
}

function normalizeLimits(limits: SnapshotLimits): SnapshotLimits {
  const maxNodes = Math.max(1, Math.min(Math.floor(limits.max_nodes), 160));
  const maxTextBytes = Math.max(
    1_024,
    Math.min(Math.floor(limits.max_text_bytes), 16 * 1024),
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
