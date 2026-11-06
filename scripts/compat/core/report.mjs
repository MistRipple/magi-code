// 兼容报告：每一项上游能力都落在下列状态之一，不允许“未分类”静默通过。
//
//   shim      兼容层自己提供实现（纯 JS 适配），不依赖 Magi 新能力
//   mapped    已有对应的 Magi 公开能力，可直接翻译
//   planned   映射已设计、Magi 有对应落点，但翻译器尚未实现；实现前翻译被阻塞
//   partial   只能覆盖上游语义的一部分（范围缩小或字段子集）；与 dropped 同等对待，必须显式接受
//   dropped   Magi 不提供；翻译后该功能丢失，必须如实告知用户
//   bundled   纯 JS 依赖，构建期打进单入口
//   unknown   没有登记的能力；必须人工评审后归类

export const STATUSES = ['mapped', 'shim', 'bundled', 'planned', 'partial', 'dropped', 'unknown'];

/**
 * @typedef {object} ReportEntry
 * @property {string} kind imports | calls | injects | events | native
 * @property {string} name
 * @property {string} status
 * @property {string} target 对应的 Magi 能力或处理方式
 * @property {string} note
 * @property {number} count
 * @property {boolean} [optional] 已证明是条件注入或有同步失败处理的注册；不改变能力状态
 * @property {string[]} files
 *
 * @typedef {object} CompatReport
 * @property {string} translator
 * @property {{ name: string, version: string }} upstream
 * @property {{ files: number, lines: number }} scanned
 * @property {ReportEntry[]} entries
 * @property {Record<string, number>} summary 各状态的条目数
 * @property {{ required: string[], blocked: ReportEntry[] }} verdict 必需能力里仍未满足的条目
 */

export function summarize(entries) {
  const summary = Object.fromEntries(STATUSES.map((status) => [status, 0]));
  for (const entry of entries) summary[entry.status] += 1;
  return summary;
}

/** 必需能力：条件分支缺席沿上游语义降级；未知能力即使位于可选分支也仍阻塞。 */
export function verdictFor(entries, required) {
  // partial 不算满足：只能覆盖一部分语义的能力，不能被当作“可完整翻译”。
  const satisfied = new Set(['mapped', 'shim', 'bundled']);
  const blocked = entries.filter((entry) => entry.status === 'unknown'
    || (required.includes(entry.category) && !entry.optional && !satisfied.has(entry.status)));
  return { required, blocked };
}

export function renderText(report) {
  const lines = [
    `翻译器：${report.translator}　上游：${report.upstream.name}@${report.upstream.version}`,
    `扫描：${report.scanned.files} 个文件，${report.scanned.lines} 行`,
    `汇总：${STATUSES.map((status) => `${status} ${report.summary[status]}`).join('　')}`,
    '',
  ];
  const internal = report.entries.filter((entry) => entry.category === 'internal');
  for (const status of ['unknown', 'planned', 'partial', 'dropped', 'shim', 'mapped', 'bundled']) {
    const group = report.entries.filter((entry) => entry.status === status && entry.category !== 'internal');
    if (group.length === 0) continue;
    lines.push(`## ${status}（${group.length}）`);
    for (const entry of group) {
      lines.push(`- ${entry.name}　×${entry.count}　→ ${entry.target}${entry.note ? `　（${entry.note}）` : ''}`);
    }
    lines.push('');
  }
  if (internal.length > 0) lines.push(`包内相对导入 ${internal.length} 个模块，构建期打包（不逐项列出）`, '');
  lines.push(`必需能力：${report.verdict.required.join('、') || '无'}`);
  lines.push(report.verdict.blocked.length === 0
    ? '结论：必需能力均可翻译。'
    : `结论：${report.verdict.blocked.length} 项阻塞翻译：${report.verdict.blocked.map((entry) => entry.name).join('、')}`);
  return lines.join('\n');
}
