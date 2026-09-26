#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const deriveScript = fileURLToPath(new URL("./derive-magi-permission-ledger.mjs", import.meta.url));
const tempRoot = await mkdtemp(join(tmpdir(), "magi-permission-ledger-golden-"));

function browserTurnRow(profile, requestId = `request-browser-${profile}`) {
  const profileName = profile[0].toUpperCase() + profile.slice(1);
  const turnId = `turn-browser-${profile}`;
  const sessionId = `session-browser-${profile}`;
  const browserSessionId = `browser-session-${profile}`;
  const tabId = `tab-browser-${profile}`;
  return {
    fixture_id: `electron-browser-permission:${profileName}:browser_snapshot`,
    schema_version: "magi.permission.fixture.v1",
    case: `electron_browser_chromium_${profileName}_read_snapshot`,
    tool: "browser_snapshot",
    browser_tool_access: "read",
    browser_requested_access: "read",
    authorization_basis: "browser_tool_access+browser_session_tab_binding",
    authorization_decision: "allow",
    authorization_scope: {
      kind: "personal_session",
      workspace_id: null,
      session_id: sessionId,
      browser_session_id: browserSessionId,
      browser_tab_id: tabId,
      turn_id: turnId,
      surface_id: `surface-browser-${profile}`,
      control_lease: null,
    },
    authorization_evidence: {
      browser_tool_access: "read",
      requested_access: "read",
      decision: "allow",
      basis: "browser_tool_access+browser_session_tab_binding",
      control_lease_event: null,
    },
    approval_requirement: "not_required",
    surface: "browser_chromium",
    access_profile: profileName,
    scope: "external_origin",
    lifecycle: "allow_read",
    side_effect: "none",
    executor_called: true,
    approval_events: [],
    provider_requests: 1,
    turn_status: "completed",
    task_status: "not_applicable",
    terminal_source: "canonical_turn_coordinator",
    status: "completed",
    request_id: requestId,
    turn_id: turnId,
    execution_profile: "conversation",
    terminal_event_sequence: 42,
    terminal_trace_id: `trace-browser-${profile}`,
  };
}

function browserWriteRow(profile) {
  const row = browserTurnRow(profile);
  const lease = {
    event_id: `event-browser-lease-${profile}`,
    event_sequence: 43,
    lease_id: `lease-browser-${profile}`,
    workspace_id: null,
    session_id: row.authorization_scope.session_id,
    turn_id: row.turn_id,
    tab_id: row.authorization_scope.browser_tab_id,
    surface_id: row.authorization_scope.surface_id,
  };
  return {
    ...row,
    fixture_id: `electron-browser-permission:${row.access_profile}:browser_click`,
    case: `electron_browser_chromium_${row.access_profile}_allow_click`,
    tool: "browser_click",
    browser_tool_access: "write",
    browser_requested_access: "write",
    authorization_basis: "browser_tool_access+surface_control_lease",
    authorization_scope: { ...row.authorization_scope, control_lease: lease },
    authorization_evidence: {
      browser_tool_access: "write",
      requested_access: "write",
      decision: "allow",
      basis: "browser_tool_access+surface_control_lease",
      control_lease_event: lease,
    },
    lifecycle: "allow_write",
  };
}

function toolRuntimeRow() {
  return {
    fixture_id: "tool-runtime:browser_host:browser_snapshot:ReadOnly:allow",
    schema_version: "magi.permission.fixture.v1",
    case: "browser_host_snapshot_read_only",
    tool: "browser_snapshot",
    surface: "browser_host",
    access_profile: "ReadOnly",
    scope: "host_surface",
    lifecycle: "allow_read",
    side_effect: "policy_only",
    executor_called: false,
    approval_events: [],
    provider_requests: 0,
    turn_status: "not_applicable",
    task_status: "not_applicable",
    terminal_source: "executor_boundary",
    status: "Succeeded",
  };
}

function apiTurnRows() {
  return [
    {
      profile: "ReadOnly",
      scope: "workspace_internal",
      surface: "git_workspace",
      tool: "git_branch_switch",
      lifecycle: "deny",
      turnStatus: "failed",
    },
    {
      profile: "Restricted",
      scope: "workspace_external",
      surface: "background_process",
      tool: "shell_exec",
      lifecycle: "approval_denied",
      turnStatus: "failed",
    },
    {
      profile: "FullAccess",
      scope: "workspace_internal",
      surface: "background_process",
      tool: "shell_exec",
      lifecycle: "allow",
      turnStatus: "completed",
    },
  ].map((row, index) => ({
    fixture_id: `api-permission:${row.profile}:${row.tool}:${index}`,
    schema_version: "magi.permission.fixture.v1",
    case: `api_${row.profile}_${row.tool}_${index}`,
    tool: row.tool,
    surface: row.surface,
    access_profile: row.profile,
    scope: row.scope,
    lifecycle: row.lifecycle,
    side_effect: row.lifecycle === "allow" ? "executed" : "blocked",
    approval_requested: row.profile === "Restricted",
    approval_resolved: row.profile === "Restricted",
    provider_requests: 0,
    turn_status: row.turnStatus,
    task_status: row.turnStatus,
    terminal_source: "canonical_turn_coordinator",
    request_id: `request-api-${index}`,
    turn_id: `turn-api-${index}`,
    execution_profile: "task",
  }));
}

async function derive(name, rows) {
  const inputPath = join(tempRoot, `${name}.input.json`);
  const outputPath = join(tempRoot, `${name}.ledger.json`);
  await writeFile(inputPath, `${JSON.stringify(rows, null, 2)}\n`, "utf8");
  const result = spawnSync(
    process.execPath,
    [deriveScript, "--output", outputPath, inputPath],
    { encoding: "utf8" },
  );
  let ledger = null;
  try {
    ledger = JSON.parse(await readFile(outputPath, "utf8"));
  } catch {
    // The subprocess result below reports an invocation or output failure.
  }
  return { result, ledger };
}

try {
  const browserRows = ["readOnly", "restricted", "fullAccess"].flatMap((profile) => [
    browserTurnRow(profile),
    browserWriteRow(profile),
  ]);
  const baseRows = apiTurnRows();
  const valid = await derive("browser-turn-valid", [...baseRows, ...browserRows]);
  assert.equal(valid.result.status, 0, `${valid.result.stderr}\n${valid.result.stdout}`);
  assert.equal(valid.ledger.status, "passed");
  assert.equal(valid.ledger.summary.turn_service_executor_row_count, 6);
  assert.equal(valid.ledger.summary.tool_runtime_row_count, 0);
  assert.equal(valid.ledger.summary.api_row_count, 3);
  assert.deepEqual(valid.ledger.summary.browser_tool_access_classes, ["read", "write"]);
  assert.deepEqual(valid.ledger.summary.browser_requested_accesses, ["read", "write"]);
  assert.equal(valid.ledger.summary.browser_authorization_decisions.allow, 6);
  assert.equal(valid.ledger.summary.browser_allowed_write_rows_with_control_lease, 3);
  assert(valid.ledger.rows.filter((row) => row.turn_id.startsWith("turn-browser-")).every(
    (row) => row.source_kind === "turn_service_executor",
  ));

  const invalidRows = browserRows.map((row) => ({ ...row }));
  const missingRequest = invalidRows[1];
  delete missingRequest.request_id;
  const invalid = await derive("browser-turn-missing-request", [...baseRows, ...invalidRows]);
  assert.equal(invalid.result.status, 2, invalid.result.stderr);
  assert.equal(invalid.ledger.status, "incomplete");
  assert(
    invalid.ledger.validation.errors.some((error) => error.includes("缺少 request_id")),
    JSON.stringify(invalid.ledger.validation.errors),
  );

  const missingToolAccessRows = browserRows.map((row) => ({ ...row }));
  delete missingToolAccessRows[1].browser_tool_access;
  const missingToolAccess = await derive("browser-turn-missing-tool-access", [
    ...baseRows,
    ...missingToolAccessRows,
  ]);
  assert.equal(missingToolAccess.result.status, 2, missingToolAccess.result.stderr);
  assert.equal(missingToolAccess.ledger.status, "incomplete");
  assert(
    missingToolAccess.ledger.validation.errors.some((error) => error.includes("缺少有效 browser_tool_access")),
    JSON.stringify(missingToolAccess.ledger.validation.errors),
  );

  const missingRequestedAccessRows = browserRows.map((row) => ({ ...row }));
  delete missingRequestedAccessRows[1].browser_requested_access;
  const missingRequestedAccess = await derive("browser-turn-missing-requested-access", [
    ...baseRows,
    ...missingRequestedAccessRows,
  ]);
  assert.equal(missingRequestedAccess.result.status, 2, missingRequestedAccess.result.stderr);
  assert.equal(missingRequestedAccess.ledger.status, "incomplete");
  assert(
    missingRequestedAccess.ledger.validation.errors.some((error) => error.includes("缺少有效 browser_requested_access")),
    JSON.stringify(missingRequestedAccess.ledger.validation.errors),
  );

  const mismatchedToolAccessRows = browserRows.map((row) => ({ ...row }));
  mismatchedToolAccessRows[0].browser_tool_access = "write";
  const mismatchedToolAccess = await derive("browser-turn-mismatched-tool-access", [
    ...baseRows,
    ...mismatchedToolAccessRows,
  ]);
  assert.equal(mismatchedToolAccess.result.status, 2, mismatchedToolAccess.result.stderr);
  assert.equal(mismatchedToolAccess.ledger.status, "incomplete");
  assert(
    mismatchedToolAccess.ledger.validation.errors.some((error) => error.includes("与 Browser 工具目录分类不一致")),
    JSON.stringify(mismatchedToolAccess.ledger.validation.errors),
  );

  const missingAuthorizationRows = browserRows.map((row) => ({ ...row }));
  delete missingAuthorizationRows[0].authorization_basis;
  const missingAuthorization = await derive("browser-turn-missing-authorization-basis", [
    ...baseRows,
    ...missingAuthorizationRows,
  ]);
  assert.equal(missingAuthorization.result.status, 2, missingAuthorization.result.stderr);
  assert(
    missingAuthorization.ledger.validation.errors.some((error) => error.includes("缺少 authorization_basis")),
    JSON.stringify(missingAuthorization.ledger.validation.errors),
  );

  const missingLeaseRows = browserRows.map((row) => ({ ...row }));
  missingLeaseRows[1].authorization_scope.control_lease = null;
  const missingLease = await derive("browser-turn-missing-control-lease", [...baseRows, ...missingLeaseRows]);
  assert.equal(missingLease.result.status, 2, missingLease.result.stderr);
  assert(
    missingLease.ledger.validation.errors.some((error) => error.includes("Browser 写授权缺少 control lease")),
    JSON.stringify(missingLease.ledger.validation.errors),
  );

  const lowerLevel = await derive("tool-runtime-only", [...baseRows, toolRuntimeRow()]);
  assert.equal(lowerLevel.result.status, 0, `${lowerLevel.result.stderr}\n${lowerLevel.result.stdout}`);
  assert.equal(lowerLevel.ledger.status, "passed");
  assert.equal(lowerLevel.ledger.summary.turn_service_executor_row_count, 0);
  assert.equal(lowerLevel.ledger.summary.tool_runtime_row_count, 1);
  assert.equal(
    lowerLevel.ledger.rows.find((row) => row.case === "browser_host_snapshot_read_only").source_kind,
    "tool_runtime",
  );

  console.log("magi permission ledger golden passed (Turn identity, BrowserToolAccess classification, authorization scope, write control lease, approval facts, executor-only separation)");
} finally {
  await rm(tempRoot, { recursive: true, force: true });
}
