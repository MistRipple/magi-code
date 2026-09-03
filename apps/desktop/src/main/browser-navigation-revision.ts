/**
 * 判断一个 Browser Surface binding 是否仍对应当前文档。
 *
 * 普通命令只能访问它发起时的文档；导航命令或已完成的交互动作则可以
 * 接受同一物理 WebContents 的文档代次向前推进，但绝不能接受回退。
 */
export function matchesNavigationRevision(
  bindingRevision: number,
  currentRevision: number,
  allowAdvance: boolean,
): boolean {
  return allowAdvance
    ? bindingRevision <= currentRevision
    : bindingRevision === currentRevision;
}
