import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  bridgeRuntime.setClientBridge({
    kind: 'web',
    postMessage() {},
    onMessage() {
      return () => {};
    },
    getState() {
      return undefined;
    },
    setState() {},
    getInitialSessionId() {
      return '';
    },
    getInitialLocale() {
      return 'zh-CN';
    },
    notifyReady() {},
  });

  const messages = await server.ssrLoadModule('/src/stores/messages.svelte.ts');
  const turns = await server.ssrLoadModule('/src/stores/turn-store.svelte.ts');
  const sessionId = 'session-history-pagination-golden';
  const workspaceId = 'workspace-history-pagination-golden';
  const workspacePath = '/tmp/workspace-history-pagination-golden';

  function prepareLoadedWindow() {
    messages.messagesState.currentWorkspaceId = workspaceId;
    messages.messagesState.currentWorkspacePath = workspacePath;
    messages.setCurrentSessionId(null);
    messages.setCurrentSessionId(sessionId);
    const projection = turns.replaceCanonicalSessionTurns(
      sessionId,
      [canonicalTurn(sessionId, 'turn-21', 21)],
    );
    assert.ok(projection, 'loaded canonical window must produce a projection');
    assert.equal(messages.setCanonicalTimelineProjection(projection), true);
    messages.setSessionHistoryState(sessionId, {
      workspaceId,
      workspacePath,
      canonicalHasMoreBefore: true,
      canonicalBeforeCursor: 'turn-21',
      historyLoadStatus: 'idle',
    });
    return messages.messagesState.sessionHistory;
  }

  function beginPage() {
    const state = messages.messagesState.sessionHistory;
    messages.setSessionHistoryState(sessionId, {
      workspaceId,
      workspacePath,
      historyLoadStatus: 'loading',
    });
    return {
      revision: state.revision,
      canonicalBeforeCursor: state.canonicalBeforeCursor,
    };
  }

  let state = prepareLoadedWindow();
  const firstPage = beginPage();
  const committed = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: firstPage.revision,
    canonicalBeforeCursor: firstPage.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-20', 20)],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-20',
  });
  assert.deepEqual(
    committed,
    { accepted: true, reason: 'committed', addedTurnCount: 1 },
    'a page with a strictly older turn and an advancing cursor must commit',
  );
  assert.deepEqual(
    turns.turnStoreState.reducer.turns.map((turn) => turn.turnId),
    ['turn-20', 'turn-21'],
    'successful pagination must prepend the new canonical turn exactly once',
  );
  assert.equal(messages.messagesState.sessionHistory.historyLoadStatus, 'idle');

  state = messages.messagesState.sessionHistory;
  const finalPage = beginPage();
  const exhausted = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: finalPage.revision,
    canonicalBeforeCursor: finalPage.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-19', 19)],
    canonicalHasMoreBefore: false,
    nextCanonicalBeforeCursor: 'turn-19',
  });
  assert.equal(exhausted.accepted, true, 'the final non-empty page must commit');
  assert.equal(messages.messagesState.sessionHistory.historyLoadStatus, 'exhausted');
  assert.equal(messages.messagesState.sessionHistory.canonicalBeforeCursor, 'turn-19');

  state = prepareLoadedWindow();
  const duplicatePage = beginPage();
  const duplicate = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: duplicatePage.revision,
    canonicalBeforeCursor: duplicatePage.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-21', 21)],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-21',
  });
  assert.equal(duplicate.accepted, false, 'a duplicate page must be rejected as no progress');
  assert.equal(duplicate.reason, 'no-progress');
  assert.equal(messages.messagesState.sessionHistory.historyLoadStatus, 'no-progress');
  assert.deepEqual(
    turns.turnStoreState.reducer.turns.map((turn) => turn.turnId),
    ['turn-21'],
    'a rejected duplicate page must not mutate the canonical projection',
  );

  state = prepareLoadedWindow();
  const futurePage = beginPage();
  const future = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: futurePage.revision,
    canonicalBeforeCursor: futurePage.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-22', 22)],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-22',
  });
  assert.equal(future.accepted, false, 'a page newer than the loaded window must be rejected');
  assert.equal(messages.messagesState.sessionHistory.historyLoadStatus, 'no-progress');

  state = prepareLoadedWindow();
  const malformedPage = beginPage();
  const duplicateTurnInPage = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: malformedPage.revision,
    canonicalBeforeCursor: malformedPage.canonicalBeforeCursor,
    turns: [
      canonicalTurn(sessionId, 'turn-20', 20),
      canonicalTurn(sessionId, 'turn-20', 20),
    ],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-20',
  });
  assert.equal(duplicateTurnInPage.accepted, false, 'a page with duplicate turn facts must be rejected');
  assert.deepEqual(
    turns.turnStoreState.reducer.turns.map((turn) => turn.turnId),
    ['turn-21'],
    'strict page validation must reject the whole page before projection mutation',
  );

  messages.setSessionHistoryState(sessionId, {
    workspaceId,
    workspacePath,
    historyLoadStatus: 'loading',
  });
  const retry = messages.messagesState.sessionHistory;
  const retryCommit = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: retry.revision,
    canonicalBeforeCursor: retry.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-20', 20)],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-20',
  });
  assert.equal(retryCommit.accepted, true, 'an explicit retry must be allowed after no progress');

  state = prepareLoadedWindow();
  const sameCursorPage = beginPage();
  const sameCursor = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: sameCursorPage.revision,
    canonicalBeforeCursor: sameCursorPage.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-20', 20)],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-21',
  });
  assert.equal(sameCursor.accepted, false, 'a page whose cursor does not advance must be rejected');
  assert.equal(messages.messagesState.sessionHistory.historyLoadStatus, 'no-progress');

  state = prepareLoadedWindow();
  const missingNextCursor = beginPage();
  const inconsistentWindow = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: missingNextCursor.revision,
    canonicalBeforeCursor: missingNextCursor.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-20', 20)],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: null,
  });
  assert.equal(inconsistentWindow.accepted, false, 'hasMore=true without a cursor must be rejected');
  assert.equal(messages.messagesState.sessionHistory.historyLoadStatus, 'no-progress');

  state = prepareLoadedWindow();
  const duplicateSeqPage = beginPage();
  const duplicateTurnSeq = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: duplicateSeqPage.revision,
    canonicalBeforeCursor: duplicateSeqPage.canonicalBeforeCursor,
    turns: [canonicalTurn(sessionId, 'turn-21-replayed', 21)],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-21-replayed',
  });
  assert.equal(duplicateTurnSeq.accepted, false, 'a page reusing an existing turn sequence must be rejected');
  assert.equal(duplicateTurnSeq.reason, 'no-progress');

  state = prepareLoadedWindow();
  const mixedOrderPage = beginPage();
  const mixedOrder = messages.commitOlderSessionHistoryPage({
    sessionId,
    workspaceId,
    workspacePath,
    revision: mixedOrderPage.revision,
    canonicalBeforeCursor: mixedOrderPage.canonicalBeforeCursor,
    turns: [
      canonicalTurn(sessionId, 'turn-20', 20),
      canonicalTurn(sessionId, 'turn-99', 99),
    ],
    canonicalHasMoreBefore: true,
    nextCanonicalBeforeCursor: 'turn-20',
  });
  assert.equal(mixedOrder.accepted, false, 'a page containing a future turn must be rejected as a whole');
  assert.equal(mixedOrder.reason, 'no-progress');

  state = prepareLoadedWindow();
  messages.setSessionHistoryState(sessionId, {
    workspaceId,
    workspacePath,
    historyLoadStatus: 'error',
  });
  messages.setSessionHistoryState(sessionId, {
    workspaceId,
    workspacePath,
    canonicalHasMoreBefore: true,
    canonicalBeforeCursor: 'turn-21',
    preserveLoadedWindow: true,
  });
  assert.equal(
    messages.messagesState.sessionHistory.historyLoadStatus,
    'error',
    'an authoritative refresh must not erase a retryable request failure',
  );
  assert.equal(state.revision <= messages.messagesState.sessionHistory.revision, true);

  state = prepareLoadedWindow();
  messages.setSessionHistoryState(sessionId, {
    workspaceId,
    workspacePath,
    historyLoadStatus: 'loading',
  });
  messages.setSessionHistoryState(sessionId, {
    workspaceId,
    workspacePath,
    canonicalHasMoreBefore: true,
    canonicalBeforeCursor: 'turn-21',
    preserveLoadedWindow: true,
  });
  assert.equal(
    messages.messagesState.sessionHistory.historyLoadStatus,
    'loading',
    '同会话 bootstrap 不得覆盖正在进行的历史分页请求',
  );
}, { configFile: 'vite.web.config.ts' });

console.log('history pagination golden checks passed');

function canonicalTurn(sessionId, turnId, turnSeq) {
  return {
    sessionId,
    turnId,
    turnSeq,
    acceptedAt: turnSeq,
    completedAt: turnSeq,
    status: 'completed',
    items: [{
      sessionId,
      turnId,
      turnSeq,
      itemId: `${turnId}-item`,
      itemSeq: 1,
      kind: 'user_message',
      createdAt: turnSeq,
      updatedAt: turnSeq,
      status: 'completed',
      content: `历史消息 ${turnSeq}`,
      sourceThreadId: `${sessionId}-thread`,
      visibility: { renderable: true },
    }],
  };
}
