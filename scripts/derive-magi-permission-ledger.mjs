#!/usr/bin/env node

import { createHash } from "node:crypto";
import { basename, dirname, resolve } from "node:path";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const SCHEMA_VERSION = "magi.permission.v5";
const DERIVE_VERSION = "magi-permission-derive.v8";
const REQUIRED_COMMON_FIELDS = [
  "case",
  "tool",
  "surface",
  "access_profile",
  "scope",
  "lifecycle",
  "side_effect",
];
const TURN_IDENTITY_FIELDS = ["request_id", "turn_id", "execution_profile"];
const EXPECTED_PROFILES = ["ReadOnly", "Restricted", "FullAccess"];
const EXPECTED_EXECUTION_PROFILES = ["conversation", "task"];
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const browserToolSchema = JSON.parse(await readFile(
  resolve(repositoryRoot, "contracts/desktop-browser/browser-tool.schema.json"),
  "utf8",
));
const browserToolAccessByName = new Map(
  (browserToolSchema["x-magi-browser-tool-catalog"] ?? [])
    .map((tool) => [tool.name, tool.access]),
);

function usage() {
  console.error("用法：node scripts/derive-magi-permission-ledger.mjs --output OUTPUT INPUT...");
}

function parseArgs(argv) {
  let output = null;
  const inputs = [];
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--output") {
      output = argv[index + 1];
      index += 1;
    } else if (value === "--help" || value === "-h") {
      usage();
      process.exit(0);
    } else {
      inputs.push(value);
    }
  }
  if (!output || inputs.length === 0) {
    usage();
    throw new Error("必须提供 --output 和至少一个 JSON 输入文件");
  }
  return { output: resolve(output), inputs: inputs.map((input) => resolve(input)) };
}

function stableValue(value) {
  if (Array.isArray(value)) return value.map(stableValue);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, stableValue(value[key])]),
    );
  }
  return value;
}

function hashJson(value) {
  return createHash("sha256")
    .update(JSON.stringify(stableValue(value)))
    .digest("hex");
}

function sourceKind(row) {
  if (Object.hasOwn(row, "approval_requested")) return "turn_service_api";
  if (["request_id", "turn_id", "execution_profile", "terminal_event_sequence"]
    .some((field) => Object.hasOwn(row, field))) return "turn_service_executor";
  if (Object.hasOwn(row, "executor_called")) return "tool_runtime";
  return "unknown";
}

function normalizeRow(row, inputPath, inputHash, rowIndex) {
  const kind = sourceKind(row);
  return {
    source_kind: kind,
    source_file: basename(inputPath),
    source_payload_hash: inputHash,
    payload_hash: hashJson(row),
    row_index: rowIndex,
    fixture_id: row.fixture_id ?? row.fixture ?? null,
    schema_version: row.schema_version ?? null,
    case: row.case ?? null,
    tool: row.tool ?? null,
    surface: row.surface ?? null,
    access_profile: row.access_profile ?? null,
    browser_tool_access: row.browser_tool_access ?? null,
    browser_requested_access: row.browser_requested_access ?? null,
    authorization_basis: row.authorization_basis ?? null,
    authorization_decision: row.authorization_decision ?? null,
    authorization_scope: row.authorization_scope ?? null,
    authorization_evidence: row.authorization_evidence ?? null,
    approval_requirement: row.approval_requirement ?? null,
    scope: row.scope ?? null,
    lifecycle: row.lifecycle ?? null,
    side_effect: row.side_effect ?? null,
    approval_requested: row.approval_requested ?? null,
    approval_resolved: row.approval_resolved ?? null,
    approval_events: row.approval_events ?? null,
    executor_called: row.executor_called ?? null,
    provider_requests: row.provider_requests ?? null,
    turn_status: row.turn_status ?? null,
    task_status: row.task_status ?? null,
    terminal_source: row.terminal_source ?? null,
    status: row.status ?? null,
    request_id: row.request_id ?? null,
    turn_id: row.turn_id ?? null,
    execution_profile: row.execution_profile ?? null,
    terminal_event_sequence: row.terminal_event_sequence ?? null,
    terminal_trace_id: row.terminal_trace_id ?? null,
  };
}

function validateRows(rows) {
  const errors = [];
  const seen = new Set();
  for (const row of rows) {
    const identity = `${row.source_file}:${row.row_index}`;
    for (const field of REQUIRED_COMMON_FIELDS) {
      if (typeof row[field] !== "string" || row[field].trim() === "") {
        errors.push(`${identity} 缺少 ${field}`);
      }
    }
    if (typeof row.fixture_id !== "string" || row.fixture_id.trim() === "") {
      errors.push(`${identity} 缺少 fixture_id`);
    }
    if (row.schema_version !== "magi.permission.fixture.v1") {
      errors.push(`${identity} 的 schema_version 无效`);
    }
    if (row.source_kind === "turn_service_executor" && row.surface === "browser_chromium") {
      if (!["read", "write", "mixed"].includes(row.browser_tool_access)) {
        errors.push(`${identity} 缺少有效 browser_tool_access`);
      } else if (browserToolAccessByName.get(row.tool) !== row.browser_tool_access) {
        errors.push(`${identity} 的 browser_tool_access 与 Browser 工具目录分类不一致`);
      }
      if (!["read", "write"].includes(row.browser_requested_access)) {
        errors.push(`${identity} 缺少有效 browser_requested_access`);
      } else if ((row.browser_tool_access === "read" && row.browser_requested_access !== "read")
        || (row.browser_tool_access === "write" && row.browser_requested_access !== "write")) {
        errors.push(`${identity} 的 browser_requested_access 与 Browser 工具访问能力不兼容`);
      }
      if (typeof row.authorization_basis !== "string" || row.authorization_basis.trim() === "") {
        errors.push(`${identity} 缺少 authorization_basis`);
      }
      if (!["allow", "deny"].includes(row.authorization_decision)) {
        errors.push(`${identity} 的 authorization_decision 无效`);
      } else if ((row.authorization_decision === "allow" && row.executor_called !== true)
        || (row.authorization_decision === "deny" && row.executor_called !== false)) {
        errors.push(`${identity} 的授权决定与 executor_called 不一致`);
      }
      if (!row.authorization_scope || typeof row.authorization_scope !== "object") {
        errors.push(`${identity} 缺少 authorization_scope`);
      } else {
        const authorizationScope = row.authorization_scope;
        if (authorizationScope.turn_id !== row.turn_id) {
          errors.push(`${identity} 的 authorization_scope.turn_id 与 Turn identity 不一致`);
        }
        if (typeof authorizationScope.session_id !== "string" || authorizationScope.session_id.trim() === "") {
          errors.push(`${identity} 缺少 authorization_scope.session_id`);
        }
        if (typeof authorizationScope.browser_tab_id !== "string" || authorizationScope.browser_tab_id.trim() === "") {
          errors.push(`${identity} 缺少 authorization_scope.browser_tab_id`);
        }
        if (authorizationScope.kind === "workspace_session"
          && (typeof authorizationScope.workspace_id !== "string" || authorizationScope.workspace_id.trim() === "")) {
          errors.push(`${identity} 的 workspace Session 缺少 workspace_id`);
        }
        if (row.authorization_decision === "allow" && row.browser_requested_access === "write") {
          const lease = authorizationScope.control_lease;
          if (!lease || typeof lease !== "object") {
            errors.push(`${identity} 的 Browser 写授权缺少 control lease`);
          } else {
            for (const field of ["event_id", "lease_id", "session_id", "turn_id", "tab_id", "surface_id"]) {
              if (typeof lease[field] !== "string" || lease[field].trim() === "") {
                errors.push(`${identity} 的 control lease 缺少 ${field}`);
              }
            }
            if (!Number.isSafeInteger(lease.event_sequence) || lease.event_sequence < 1) {
              errors.push(`${identity} 的 control lease 缺少有效 event_sequence`);
            }
            if (lease.session_id !== authorizationScope.session_id
              || lease.turn_id !== row.turn_id
              || lease.tab_id !== authorizationScope.browser_tab_id
              || lease.surface_id !== authorizationScope.surface_id
              || (lease.workspace_id ?? null) !== (authorizationScope.workspace_id ?? null)) {
              errors.push(`${identity} 的 control lease 与授权 scope 不一致`);
            }
          }
        }
      }
      const authorizationEvidence = row.authorization_evidence;
      if (!authorizationEvidence || typeof authorizationEvidence !== "object") {
        errors.push(`${identity} 缺少 authorization_evidence`);
      } else {
        if (authorizationEvidence.browser_tool_access !== row.browser_tool_access) {
          errors.push(`${identity} 的 authorization_evidence BrowserToolAccess 与目录分类不一致`);
        }
        if (authorizationEvidence.requested_access !== row.browser_requested_access) {
          errors.push(`${identity} 的 authorization_evidence requested_access 不一致`);
        }
        if (authorizationEvidence.decision !== row.authorization_decision) {
          errors.push(`${identity} 的 authorization_evidence decision 不一致`);
        }
        if (authorizationEvidence.basis !== row.authorization_basis) {
          errors.push(`${identity} 的 authorization_evidence basis 不一致`);
        }
        if (row.authorization_decision === "allow" && row.browser_requested_access === "write"
          && (!authorizationEvidence.control_lease_event
            || authorizationEvidence.control_lease_event.lease_id
              !== row.authorization_scope?.control_lease?.lease_id)) {
          errors.push(`${identity} 的 authorization_evidence 未关联实际 control lease event`);
        }
      }
      const browserApprovalEvents = Array.isArray(row.approval_events) ? row.approval_events : [];
      if (!["required", "not_required"].includes(row.approval_requirement)) {
        errors.push(`${identity} 缺少有效 approval_requirement`);
      } else if (row.approval_requirement === "not_required" && browserApprovalEvents.length !== 0) {
        errors.push(`${identity} 标记不需审批但记录了 approval event`);
      } else if (row.approval_requirement === "required" && browserApprovalEvents.length === 0) {
        errors.push(`${identity} 策略要求审批但缺少 approval event`);
      }
    } else if (row.browser_tool_access !== null
      && !["read", "write", "mixed"].includes(row.browser_tool_access)) {
      errors.push(`${identity} 的 browser_tool_access 无效`);
    } else if (row.browser_requested_access !== null
      && !["read", "write"].includes(row.browser_requested_access)) {
      errors.push(`${identity} 的 browser_requested_access 无效`);
    }
    if (typeof row.terminal_source !== "string" || row.terminal_source.trim() === "") {
      errors.push(`${identity} 缺少 terminal_source`);
    }
    if (!/^[0-9a-f]{64}$/u.test(row.source_payload_hash || "")) {
      errors.push(`${identity} 的 source_payload_hash 无效`);
    }
    if (!/^[0-9a-f]{64}$/u.test(row.payload_hash || "")) {
      errors.push(`${identity} 的 payload_hash 无效`);
    }
    if (row.source_kind === "turn_service_api") {
      for (const field of ["approval_requested", "approval_resolved"]) {
        if (typeof row[field] !== "boolean") errors.push(`${identity} 的 ${field} 不是布尔值`);
      }
      for (const field of ["request_id", "turn_id", "execution_profile"]) {
        if (typeof row[field] !== "string" || row[field].trim() === "") {
          errors.push(`${identity} 缺少 ${field}`);
        }
      }
      if (row.execution_profile !== "task") {
        errors.push(`${identity} 的 execution_profile 必须为 task`);
      }
      if (!Number.isSafeInteger(row.provider_requests) || row.provider_requests < 0) {
        errors.push(`${identity} 的 provider_requests 不是非负整数`);
      }
      for (const field of ["turn_status", "task_status"]) {
        if (typeof row[field] !== "string" || row[field].trim() === "") {
          errors.push(`${identity} 缺少 ${field}`);
        }
      }
    } else if (row.source_kind === "turn_service_executor") {
      for (const field of TURN_IDENTITY_FIELDS) {
        if (typeof row[field] !== "string" || row[field].trim() === "") {
          errors.push(`${identity} 缺少 ${field}`);
        }
      }
      if (!EXPECTED_EXECUTION_PROFILES.includes(row.execution_profile)) {
        errors.push(`${identity} 的 execution_profile 无效`);
      }
      if (typeof row.executor_called !== "boolean") errors.push(`${identity} 的 executor_called 不是布尔值`);
      if (!Array.isArray(row.approval_events)) errors.push(`${identity} 的 approval_events 不是数组`);
      if (!Number.isSafeInteger(row.provider_requests) || row.provider_requests < 0) {
        errors.push(`${identity} 的 provider_requests 不是非负整数`);
      }
      for (const field of ["turn_status", "task_status"]) {
        if (typeof row[field] !== "string" || row[field].trim() === "") {
          errors.push(`${identity} 缺少 ${field}`);
        }
      }
      if (!Number.isSafeInteger(row.terminal_event_sequence) || row.terminal_event_sequence < 1) {
        errors.push(`${identity} 缺少有效 terminal_event_sequence`);
      }
      if (typeof row.terminal_trace_id !== "string" || row.terminal_trace_id.trim() === "") {
        errors.push(`${identity} 缺少 terminal_trace_id`);
      }
      if (typeof row.status !== "string" || row.status.trim() === "") errors.push(`${identity} 缺少 status`);
    } else if (row.source_kind === "tool_runtime") {
      if (typeof row.executor_called !== "boolean") errors.push(`${identity} 的 executor_called 不是布尔值`);
      if (row.provider_requests !== null
        && (!Number.isSafeInteger(row.provider_requests) || row.provider_requests < 0)) {
        errors.push(`${identity} 的 provider_requests 不是非负整数或 null`);
      }
      if (!Array.isArray(row.approval_events)) errors.push(`${identity} 的 approval_events 不是数组`);
      if (typeof row.status !== "string" || row.status.trim() === "") errors.push(`${identity} 缺少 status`);
    } else {
      errors.push(`${identity} 无法识别 artifact 来源类型`);
    }
    const duplicateKey = [row.source_kind, row.case, row.tool, row.access_profile].join("|");
    if (seen.has(duplicateKey)) errors.push(`重复权限矩阵行：${duplicateKey}`);
    seen.add(duplicateKey);
  }
  return errors;
}

function missingValues(rows, field, expected) {
  const observed = new Set(rows.map((row) => row[field]).filter((value) => value !== null));
  return expected.filter((value) => !observed.has(value));
}

async function main() {
  const { output, inputs } = parseArgs(process.argv.slice(2));
  const rows = [];
  const inputsMetadata = [];
  for (const inputPath of inputs) {
    const raw = await readFile(inputPath, "utf8");
    const input = JSON.parse(raw);
    const values = Array.isArray(input) ? input : input.rows;
    if (!Array.isArray(values)) throw new Error(`输入不是 JSON 数组或 rows 数组：${inputPath}`);
    const payloadHash = hashJson(input);
    inputsMetadata.push({ path: inputPath, payload_hash: payloadHash, row_count: values.length });
    rows.push(...values.map((row, rowIndex) => normalizeRow(row, inputPath, payloadHash, rowIndex)));
  }

  const validationErrors = validateRows(rows);
  const apiRows = rows.filter((row) => row.source_kind === "turn_service_api");
  const turnExecutorRows = rows.filter((row) => row.source_kind === "turn_service_executor");
  const browserTurnRows = turnExecutorRows.filter((row) => row.surface === "browser_chromium");
  const runtimeRows = rows.filter((row) => row.source_kind === "tool_runtime");
  const gaps = {
    rows_without_fixture: rows.filter((row) => row.fixture_id === null).length,
    rows_without_row_schema_version: rows.filter((row) => row.schema_version === null).length,
    rows_without_terminal_source: rows.filter((row) => row.terminal_source === null).length,
    api_missing_profiles: missingValues(apiRows, "access_profile", EXPECTED_PROFILES),
    turn_executor_missing_profiles: turnExecutorRows.length === 0
      ? []
      : missingValues(turnExecutorRows, "access_profile", EXPECTED_PROFILES),
    browser_turn_rows_without_tool_access: browserTurnRows.filter(
      (row) => row.browser_tool_access === null,
    ).length,
    browser_turn_rows_without_requested_access: browserTurnRows.filter(
      (row) => row.browser_requested_access === null,
    ).length,
    api_missing_scopes: missingValues(apiRows, "scope", ["workspace_internal", "workspace_external"]),
    api_missing_surfaces: missingValues(apiRows, "surface", ["git_workspace", "background_process"]),
    runtime_rows_without_provider_count: runtimeRows.filter((row) => row.provider_requests === null).length,
    runtime_rows_without_turn_task_terminal: runtimeRows.filter(
      (row) => row.turn_status === null || row.task_status === null,
    ).length,
  };
  const gapCount = Object.values(gaps).reduce(
    (count, value) => count + (Array.isArray(value) ? value.length : value),
    0,
  );
  const ledger = {
    schema_version: SCHEMA_VERSION,
    derive_version: DERIVE_VERSION,
    generated_at: new Date().toISOString(),
    status: validationErrors.length === 0 && gapCount === 0 ? "passed" : "incomplete",
    inputs: inputsMetadata,
    summary: {
      row_count: rows.length,
      api_row_count: apiRows.length,
      turn_service_executor_row_count: turnExecutorRows.length,
      tool_runtime_row_count: runtimeRows.length,
      access_profiles: [...new Set(rows.map((row) => row.access_profile).filter(Boolean))].sort(),
      browser_tool_access_classes: [...new Set(browserTurnRows
        .map((row) => row.browser_tool_access)
        .filter(Boolean))].sort(),
      browser_requested_accesses: [...new Set(browserTurnRows
        .map((row) => row.browser_requested_access)
        .filter(Boolean))].sort(),
      browser_authorization_decisions: Object.fromEntries(["allow", "deny"].map((decision) => [
        decision,
        browserTurnRows.filter((row) => row.authorization_decision === decision).length,
      ])),
      browser_approval_requirements: Object.fromEntries(["required", "not_required"].map((requirement) => [
        requirement,
        browserTurnRows.filter((row) => row.approval_requirement === requirement).length,
      ])),
      browser_workspace_scoped_row_count: browserTurnRows.filter(
        (row) => row.authorization_scope?.kind === "workspace_session",
      ).length,
      browser_allowed_write_rows_with_control_lease: browserTurnRows.filter(
        (row) => row.authorization_decision === "allow"
          && row.browser_requested_access === "write"
          && Boolean(row.authorization_scope?.control_lease?.lease_id),
      ).length,
      surfaces: [...new Set(rows.map((row) => row.surface).filter(Boolean))].sort(),
      scopes: [...new Set(rows.map((row) => row.scope).filter(Boolean))].sort(),
    },
    validation: { errors: validationErrors, gaps },
    rows,
  };
  await writeFile(output, `${JSON.stringify(ledger, null, 2)}\n`, "utf8");
  console.log(JSON.stringify({
    status: ledger.status,
    output,
    rowCount: rows.length,
    validationErrors: validationErrors.length,
    gaps,
  }, null, 2));
  if (ledger.status !== "passed") process.exitCode = 2;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
});
