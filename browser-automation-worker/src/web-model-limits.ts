import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Web 侧上限表（设计基线 §5.9.2）。
 *
 * 数据文件 `web-model-limits.json` 随应用发布，owner 是 `browser-automation-worker`：
 * 阶段 0 用首尾标记法实测后整体替换，daemon 不维护第二份上限数据。
 */

export interface WebModelLimitEntry {
  inputTokenBudget: number;
  composerCharLimit: number;
  singleSubmissionTokenBudget: number;
  responseReserve: number;
  tokenizerRevision: string;
  limitBehaviour: "error" | "attachment" | "silent_truncate";
}

export interface WebModelLimitsTable {
  revision: string;
  defaults: WebModelLimitEntry;
  entries: Array<{
    accountHint: string;
    family: string;
    effort: string;
    connector: boolean;
    limits: WebModelLimitEntry;
  }>;
}

interface RawLimitsFile {
  revision?: unknown;
  defaults?: unknown;
  entries?: unknown;
}

function positiveInteger(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.floor(value) : fallback;
}

function stringValue(value: unknown, fallback: string): string {
  return typeof value === "string" && value.trim() ? value : fallback;
}

function limitEntry(raw: unknown, fallback: WebModelLimitEntry): WebModelLimitEntry {
  const value = (raw ?? {}) as Record<string, unknown>;
  const behaviour = stringValue(value.limit_behaviour, fallback.limitBehaviour);
  return {
    inputTokenBudget: positiveInteger(value.input_token_budget, fallback.inputTokenBudget),
    composerCharLimit: positiveInteger(value.composer_char_limit, fallback.composerCharLimit),
    singleSubmissionTokenBudget: positiveInteger(
      value.single_submission_token_budget,
      fallback.singleSubmissionTokenBudget,
    ),
    responseReserve: positiveInteger(value.response_reserve, fallback.responseReserve),
    tokenizerRevision: stringValue(value.tokenizer_revision, fallback.tokenizerRevision),
    limitBehaviour: behaviour === "attachment" || behaviour === "silent_truncate" ? behaviour : "error",
  };
}

const FALLBACK: WebModelLimitEntry = {
  inputTokenBudget: 32_000,
  composerCharLimit: 210_000,
  singleSubmissionTokenBudget: 16_000,
  responseReserve: 8_000,
  tokenizerRevision: "o200k",
  limitBehaviour: "error",
};

/** 读取随应用发布的上限表。文件缺失或损坏时退回保守默认值，绝不抬高窗口。 */
export function loadWebModelLimits(): WebModelLimitsTable {
  let raw: RawLimitsFile = {};
  try {
    const here = dirname(fileURLToPath(import.meta.url));
    raw = JSON.parse(readFileSync(join(here, "web-model-limits.json"), "utf8")) as RawLimitsFile;
  } catch {
    raw = {};
  }
  const defaults = limitEntry(raw.defaults, FALLBACK);
  const entries = Array.isArray(raw.entries)
    ? raw.entries.flatMap((item) => {
      const value = (item ?? {}) as Record<string, unknown>;
      const accountHint = stringValue(value.account_hint, "");
      const family = stringValue(value.family, "");
      if (!accountHint || !family) return [];
      return [{
        accountHint,
        family,
        effort: stringValue(value.effort, "*"),
        connector: value.connector === true,
        limits: limitEntry(value, defaults),
      }];
    })
    : [];
  return { revision: stringValue(raw.revision, "unknown"), defaults, entries };
}

/**
 * 按 `账号等级 × 模型族 × effort × 是否附带连接器` 查表（设计基线 §5.9.2）。
 *
 * 未命中时退回 defaults，绝不返回比 defaults 更宽的窗口。
 */
export function lookupWebModelLimits(
  table: WebModelLimitsTable,
  query: { accountHint: string; family: string; effort?: string | null; connector?: boolean },
): WebModelLimitEntry {
  const effort = query.effort ?? "*";
  const connector = query.connector === true;
  const exact = table.entries.find((entry) =>
    entry.accountHint === query.accountHint
    && entry.family === query.family
    && entry.effort === effort
    && entry.connector === connector);
  if (exact) return exact.limits;
  const relaxed = table.entries.find((entry) =>
    entry.accountHint === query.accountHint
    && entry.family === "*"
    && entry.effort === "*"
    && entry.connector === connector);
  return relaxed?.limits ?? table.defaults;
}
