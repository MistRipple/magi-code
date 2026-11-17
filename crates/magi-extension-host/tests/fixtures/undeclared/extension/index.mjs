// input 只有 guard 模式：未在清单声明就绑定必须失败（观察钩子才允许按需订阅）。
export default async function activate(api) {
  await api.on('input', () => {});
}
