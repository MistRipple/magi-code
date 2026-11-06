// 页面运行时入口：配置由包内 ui/dsh-config.js 先行写入 globalThis.__MAGI_DSH_CLIENT__。
// 配置是数据（路由表、上游界面脚本名），页面运行时本身对所有 DSH 插件相同，随 Magi 发行并预先构建。
import { startDshClient } from './dsh-client-host.mjs';

const config = globalThis.__MAGI_DSH_CLIENT__;
if (!config || config.runtimeVersion !== 2) throw new Error('缺少 ui/dsh-config.js：页面运行时需要翻译器写入的路由表');
startDshClient(config);
