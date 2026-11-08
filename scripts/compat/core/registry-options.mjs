// registry 的读取恢复只交给 pacote 的 HTTP 层；不重跑翻译或安装提交。
export const registryOptions = registry => ({
  registry, fullMetadata: true, preferOnline: true, ignoreScripts: true,
  fetchRetries: 2, fetchRetryMintimeout: 250, fetchRetryMaxtimeout: 1000, fetchRetryFactor: 4,
  timeout: 30_000,
});
