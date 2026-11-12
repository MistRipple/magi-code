/**
 * 只把 daemon 明确报告为损坏、且位于 Magi 状态根内的 JSON 状态文件
 * 归类为本地配置启动问题。守护进程版本/build 不匹配等故障继续走通用提示。
 */
export function diagnoseUserStateFileStartupFailure(
  errorText: string,
  recentOutput: string,
  stateRoot: string,
): { filePath: string } | null {
  const marker = "持久状态文件损坏，拒绝以空状态继续";
  const output = `${errorText}\n${recentOutput}`.replaceAll("\\", "/");
  const markerIndex = output.lastIndexOf(marker);
  if (markerIndex < 0) return null;

  const normalizedRoot = stateRoot.replaceAll("\\", "/").replace(/\/+$/u, "");
  if (!normalizedRoot) return null;
  const rootPrefix = `${normalizedRoot}/`;
  const fileStart = output.indexOf(rootPrefix, markerIndex + marker.length);
  if (fileStart < 0) return null;

  const tail = output.slice(fileStart);
  const extensionEnd = tail.search(/\.json\b/iu);
  if (extensionEnd < 0) return null;

  const filePath = tail.slice(0, extensionEnd + ".json".length).trim();
  return filePath.startsWith(rootPrefix) ? { filePath } : null;
}

/**
 * daemon 只接受当前版本的 state layout：本机数据被更新版本的 Magi 升级过（或版本
 * 未知）时会确定性拒绝启动。重试或重启不会改变结果，调用方据此不再提供“重试”。
 */
export function diagnoseUnsupportedStateLayout(
  errorText: string,
  recentOutput: string,
): { layoutVersion: number } | null {
  const match = /不支持的 state layout 版本: (\d+)/u.exec(`${errorText}\n${recentOutput}`);
  return match ? { layoutVersion: Number(match[1]) } : null;
}
