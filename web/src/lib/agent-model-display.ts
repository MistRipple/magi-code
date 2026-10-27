/** 只展示派发回执或 Task 快照的实际选择，不读取当前角色配置补写历史。 */
export function agentModelDisplay(
  selection: { model?: unknown; effective_effort?: unknown } | null | undefined,
  translate: (key: string) => string,
): { model: string; effort: string; label: string } {
  const model = typeof selection?.model === 'string' ? selection.model.trim() : '';
  let effort = '';
  if (model && selection && 'effective_effort' in selection) {
    const value = selection.effective_effort;
    if (value == null) effort = translate('input.mainModelPicker.reasoning.providerDefault');
    else if (typeof value === 'string' && ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra'].includes(value)) {
      effort = translate(`input.mainModelPicker.reasoning.${value}`);
    }
  }
  return { model, effort, label: [model, effort].filter(Boolean).join(' · ') };
}

export function agentModelLabel(
  selection: Parameters<typeof agentModelDisplay>[0],
  translate: Parameters<typeof agentModelDisplay>[1],
): string {
  return agentModelDisplay(selection, translate).label;
}
