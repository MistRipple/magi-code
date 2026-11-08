// 原包 Remote 与 fetch 共用视图请求的身份、取消及文档回收；
// AbortSignal 只进入本地传输，不进入上游 JSON 业务参数。
export function createRemoteConnection(request) {
  return { rpc: { async call(path, endpoint, input, signal) {
    if (path !== '/api' || !input || Object.keys(input).some(key => key !== 'args')) {
      throw new Error('插件 Remote 仅支持 /api 的 JSON unary 调用');
    }
    const result = await request({ operation: 'route.call', method: 'POST',
      path: '/__magi_compat/remote', body: { endpoint, args: input.args } }, signal);
    return result.body;
  } } };
}
