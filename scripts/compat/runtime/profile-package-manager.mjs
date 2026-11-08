// profileContext 指定的私有包管理入口，只接受所属实例签发的委托。
import { callProfileProcess } from './profile-process-call.mjs';
const { result } = await callProfileProcess('install', process.argv.slice(2));
if (result.application === 'applied') console.log('Magi daemon 已提交插件管理操作');
else { console.error(result.error?.message ?? `安装结果：${result.application}`); process.exitCode = 1; }
