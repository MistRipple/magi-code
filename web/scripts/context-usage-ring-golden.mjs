import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const ring = await server.ssrLoadModule('/src/lib/context-usage-ring.ts');
  const contract = await server.ssrLoadModule('/src/shared/bridges/rust-daemon-contract.ts');

  // 简易翻译桩：回显 key 并插值，方便断言 tooltip 组合结果。
  const t = (key, params) => {
    const map = {
      'input.contextRing.label': '上下文',
      'input.contextRing.empty': '暂无上下文用量',
      'input.contextRing.usage': `已用 ${params?.value}`,
      'input.contextRing.remaining': `剩余 ${params?.value}`,
      'input.contextRing.limit': `窗口 ${params?.value}`,
    };
    return map[key] ?? key;
  };

  // 场景 1：无数据（usageRatio 缺失）应进入占位态。
  {
    const view = ring.resolveRingView({});
    assert.equal(view.hasData, false, 'missing ratio must be treated as empty');
    assert.equal(view.percentText, '--', 'empty state percent text is a placeholder');
    assert.equal(view.labelText, '--', 'empty label has no % suffix');
    assert.equal(view.tone, 'normal', 'empty state defaults to normal tone');
    assert.equal(view.geometry.dashOffset, view.geometry.circumference,
      'empty ring draws zero progress (offset == circumference)');
    assert.equal(ring.buildRingTooltip({}, t), '暂无上下文用量',
      'empty tooltip shows the dedicated empty copy');
  }

  // 场景 2：null/NaN/Infinity 也算无数据。
  for (const bad of [null, undefined, NaN, Infinity, -Infinity]) {
    assert.equal(ring.hasUsageData(bad), false, `ratio ${String(bad)} must be invalid`);
    assert.equal(ring.formatRingPercent(bad), '--', `percent for ${String(bad)} is placeholder`);
  }

  // 场景 3：正常用量四舍五入到整百分比。
  {
    const view = ring.resolveRingView({
      usageRatio: 0.426,
      tokenUsed: 4260,
      remainingTokens: 5740,
      tokenLimit: 10000,
      warningLevel: 'normal',
    });
    assert.equal(view.hasData, true);
    assert.equal(view.percentText, '43', '0.426 rounds to 43');
    assert.equal(view.labelText, '43%');
    assert.equal(view.tone, 'normal');
    assert.equal(
      ring.buildRingTooltip({
        usageRatio: 0.426,
        tokenUsed: 4260,
        remainingTokens: 5740,
        tokenLimit: 10000,
      }, t),
      '上下文 43% · 已用 4.3k · 剩余 5.7k · 窗口 10.0k',
      'normal tooltip composes label/usage/remaining/limit with k formatting',
    );
  }

  // 场景 3.1：小占用也必须按真实窗口比例显示，不能因为 baseline 语义压成 0%。
  {
    const view = ring.resolveRingView({
      usageRatio: 9_600 / 272_000,
      tokenUsed: 9_600,
      remainingTokens: 262_400,
      tokenLimit: 272_000,
      warningLevel: 'normal',
    });
    assert.equal(view.percentText, '4', '9.6k / 272k rounds to 4%, not 0%');
    assert.equal(view.labelText, '4%');
    assert.equal(
      ring.buildRingTooltip({
        usageRatio: 9_600 / 272_000,
        tokenUsed: 9_600,
        remainingTokens: 262_400,
        tokenLimit: 272_000,
      }, t),
      '上下文 4% · 已用 9.6k · 剩余 262.4k · 窗口 272.0k',
      'small but non-zero context usage must remain visible',
    );
  }

  // 场景 3.2：极小但非零的占用显示为 <1%，避免用户误解为完全未占用。
  {
    const view = ring.resolveRingView({
      usageRatio: 336 / 272_000,
      tokenUsed: 336,
      remainingTokens: 271_664,
      tokenLimit: 272_000,
      warningLevel: 'normal',
    });
    assert.equal(view.percentText, '<1', 'non-zero usage below 1% renders as <1');
    assert.equal(view.labelText, '<1%');
    assert.equal(
      ring.buildRingTooltip({
        usageRatio: 336 / 272_000,
        tokenUsed: 336,
        remainingTokens: 271_664,
        tokenLimit: 272_000,
      }, t),
      '上下文 <1% · 已用 336 · 剩余 271.7k · 窗口 272.0k',
      'tiny non-zero usage keeps precise token details in tooltip',
    );
  }

  // 场景 3.3：如果 DTO 短暂出现 tokenUsed > 0 但 usageRatio 为 0，
  // 展示层用 tokenUsed/tokenLimit 收敛成一致口径，避免再次显示 0%。
  {
    const view = ring.resolveRingView({
      usageRatio: 0,
      tokenUsed: 9_600,
      remainingTokens: 262_400,
      tokenLimit: 272_000,
      warningLevel: 'normal',
    });
    assert.equal(view.percentText, '4', 'inconsistent zero ratio derives from token fields');
    assert.equal(
      ring.buildRingTooltip({
        usageRatio: 0,
        tokenUsed: 9_600,
        remainingTokens: 262_400,
        tokenLimit: 272_000,
      }, t),
      '上下文 4% · 已用 9.6k · 剩余 262.4k · 窗口 272.0k',
      'tooltip follows the same derived effective ratio',
    );
  }

  // 场景 4：占比裁剪到 [0,1]，越界不应溢出。
  {
    assert.equal(ring.clampUsageRatio(1.5), 1, 'ratio above 1 clamps to 1');
    assert.equal(ring.clampUsageRatio(-0.3), 0, 'negative ratio clamps to 0');
    const full = ring.resolveRingView({ usageRatio: 1.5, warningLevel: 'danger' });
    assert.equal(full.percentText, '100', 'clamped ratio renders 100');
    assert.ok(Math.abs(full.geometry.dashOffset) < 1e-9,
      'full ring offset is ~0 (complete circle)');
  }

  // 场景 5：各 warningLevel 映射到正确 tone；未知值回落 normal。
  {
    assert.equal(ring.resolveRingTone('notice'), 'notice');
    assert.equal(ring.resolveRingTone('warning'), 'warning');
    assert.equal(ring.resolveRingTone('danger'), 'danger');
    assert.equal(ring.resolveRingTone('exploded'), 'normal',
      'unknown warning level falls back to normal');
    assert.equal(ring.resolveRingTone(null), 'normal');
    assert.equal(ring.resolveRingTone(undefined), 'normal');
  }

  // 场景 6：token 格式化的边界（<1000 原样、千位转 k、缺失占位）。
  {
    assert.equal(ring.formatRingTokens(0), '0', 'zero tokens render as 0, not placeholder');
    assert.equal(ring.formatRingTokens(999), '999');
    assert.equal(ring.formatRingTokens(1000), '1.0k');
    assert.equal(ring.formatRingTokens(128000), '128.0k');
    assert.equal(ring.formatRingTokens(null), '--');
    assert.equal(ring.formatRingTokens(undefined), '--');
    assert.equal(ring.formatRingTokens(NaN), '--');
  }

  // 场景 7：tooltip 在 token 字段缺失时回退占位，而非崩溃。
  {
    const tip = ring.buildRingTooltip({ usageRatio: 0.1 }, t);
    assert.equal(tip, '上下文 10% · 已用 -- · 剩余 -- · 窗口 --',
      'missing token fields degrade to -- inside the tooltip');
  }

  // 场景 8：几何随半径变化保持自洽（offset 在 [0, circumference]）。
  {
    for (const ratio of [0, 0.25, 0.5, 0.75, 1]) {
      const geo = ring.computeRingGeometry(ratio, 7);
      assert.ok(geo.dashOffset >= -1e-9 && geo.dashOffset <= geo.circumference + 1e-9,
        `offset stays within track for ratio ${ratio}`);
      const expected = geo.circumference * (1 - ratio);
      assert.ok(Math.abs(geo.dashOffset - expected) < 1e-9,
        `offset matches (1 - ratio) * circumference for ratio ${ratio}`);
    }
    const custom = ring.computeRingGeometry(0.5, 10);
    assert.ok(Math.abs(custom.circumference - 2 * Math.PI * 10) < 1e-9,
      'circumference honors a custom radius');
  }

  // 场景 9：详情浮层使用的行数据与 tooltip 共用同一格式化口径。
  {
    const items = ring.buildRingDetailItems({
      usageRatio: 0.25,
      tokenUsed: 68_000,
      remainingTokens: 204_000,
      tokenLimit: 272_000,
    }, t);
    assert.deepEqual(
      items,
      [
        { key: 'usage', text: '已用 68.0k' },
        { key: 'remaining', text: '剩余 204.0k' },
        { key: 'limit', text: '窗口 272.0k' },
      ],
      'detail popover rows must preserve usage/remaining/limit order and formatting',
    );
    assert.deepEqual(
      ring.buildRingDetailItems({}, t),
      [],
      'empty ring has no detail rows and renders the dedicated empty copy instead',
    );
  }

  // 场景 10：后端 session.budget 必须稳定映射到前端 runtimeSnapshot.budgetState。
  {
    const normalized = contract.normalizeRustBootstrapPayload({
      generatedAt: 1_780_000_000_000,
      currentSession: {
        sessionId: 'session-context-budget',
        title: '上下文预算会话',
        createdAt: 1_780_000_000_000,
        updatedAt: 1_780_000_000_001,
        messageCount: 2,
        workspaceId: 'workspace-context-budget',
      },
      sessions: [
        {
          sessionId: 'session-context-budget',
          title: '上下文预算会话',
          createdAt: 1_780_000_000_000,
          updatedAt: 1_780_000_000_001,
          messageCount: 2,
          workspaceId: 'workspace-context-budget',
        },
      ],
      workspaces: [
        {
          workspaceId: 'workspace-context-budget',
          rootPath: '/tmp/context-budget',
          displayName: 'context-budget',
          lastOpenedAt: 1_780_000_000_000,
        },
      ],
      runtimeReadModel: {
        details: {
          sessions: [
            {
              session_id: 'session-context-budget',
              latest_event_type: 'model.usage.recorded',
              current_status: 'idle',
              budget: {
                token_used: 68_000,
                remaining_tokens: 204_000,
                token_limit: 272_000,
                percent_remaining: 75,
                usage_ratio: 0.25,
                warning_level: 'notice',
              },
            },
          ],
          assignments: [],
          tasks: [],
        },
      },
      agent: { runtimeEpoch: 'context-budget-runtime' },
    });

    assert.deepEqual(
      normalized.orchestratorRuntimeState?.runtimeSnapshot?.budgetState,
      {
        tokenUsed: 68_000,
        remainingTokens: 204_000,
        tokenLimit: 272_000,
        usageRatio: 0.25,
        warningLevel: 'notice',
      },
      'bootstrap budget must become the active session runtime snapshot budgetState',
    );
  }

  console.log('context-usage-ring-golden: all scenarios passed');
});
