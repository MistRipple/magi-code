import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const source = await server.ssrLoadModule('/src/lib/mermaid-source.ts');

  assert.deepEqual(
    source.mermaidRenderSourceCandidates('flowchart TD\nA[正常节点] --> B[结束]'),
    ['flowchart TD\nA[正常节点] --> B[结束]'],
    '普通 Mermaid 源码不应被无意义改写',
  );

  assert.deepEqual(
    source.mermaidRenderSourceCandidates('flowchart TD\nA[后端 (Rust/Axum)] --> B[前端 Web/Svelte]'),
    [
      'flowchart TD\nA[后端 (Rust/Axum)] --> B[前端 Web/Svelte]',
      'flowchart TD\nA["后端 (Rust/Axum)"] --> B["前端 Web/Svelte"]',
    ],
    '带括号或斜线的 flowchart 节点标签应提供带引号的重试源码',
  );

  assert.deepEqual(
    source.mermaidRenderSourceCandidates('flowchart TD\nA[主线：用户请求] --> B{是否进入 Agent/Skill?}'),
    [
      'flowchart TD\nA[主线：用户请求] --> B{是否进入 Agent/Skill?}',
      'flowchart TD\nA["主线：用户请求"] --> B{"是否进入 Agent/Skill?"}',
    ],
    '中文冒号和决策节点里的斜线应提供带引号的重试源码',
  );

  assert.equal(
    source.repairFlowchartRiskyLabels('sequenceDiagram\nA->>B: 后端 (Rust/Axum)'),
    'sequenceDiagram\nA->>B: 后端 (Rust/Axum)',
    '非 flowchart 图表不应走 flowchart 标签修复',
  );

  assert.equal(
    source.repairFlowchartRiskyLabels('flowchart TD\nsubgraph S[系统 (System)]\nA[API/SSE]\nend'),
    'flowchart TD\nsubgraph S[系统 (System)]\nA["API/SSE"]\nend',
    'subgraph 声明暂不改写，避免破坏 Mermaid 的子图语法',
  );

  console.log('mermaid source golden replay passed');
});
