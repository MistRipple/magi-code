// DSH 运行期技能声明进入 daemon 的同一贡献目录。这里不扫描文件、不持久化，
// 也不维护另一份技能注册表；初始化、配置替换和撤销沿真实 Cordis effect。
import { runtimeId } from '../core/identity.mjs';

const fields = new Set(['name', 'description', 'content', 'source', 'invocation']);
function contribution(input) {
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new TypeError('技能声明必须为对象');
  for (const key of Object.keys(input)) if (!fields.has(key)) throw new Error(`技能字段 ${key} 尚未接入 Magi 公开合同`);
  const { name, description, content, source, invocation } = input;
  if (typeof name !== 'string' || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name) || Buffer.byteLength(name) > 128) throw new Error('技能名称无效或超长');
  if (typeof description !== 'string' || !description.trim() || Buffer.byteLength(description) > 4096) throw new Error('技能说明为空或超长');
  if (typeof content !== 'string' || !content.trim() || Buffer.byteLength(content) > 65536) throw new Error('技能正文为空或超长');
  if (source !== undefined && typeof source !== 'string') throw new TypeError('技能来源必须为字符串');
  if (invocation !== undefined && (!invocation || typeof invocation !== 'object' || Array.isArray(invocation)
    || invocation.modelInvocable !== true || invocation.userInvocable !== true
    || Object.keys(invocation).some(key => !['modelInvocable', 'userInvocable'].includes(key)))) {
    throw new Error('该技能的调用策略尚未接入 Magi 公开合同');
  }
  return { id: runtimeId('skill', name), title: name, description, instruction: content };
}

export async function installSkills(ctx, register) {
  // 记录器先建立发行运行库的依赖解析；此时再加载 Cordis，
  // 不能在模块导入时提前锁定脚本物理目录中的版本。
  const { Service } = await import('@deepseek-ai/cordis');
  // 只保留本实例 effect 的注册意图以实现上游同层 first-wins；正文和生效
  // 目录仍只提交给 daemon，重复注册的释放函数不能撤掉原有贡献。
  const names = new Set();
  return ctx.plugin(class extends Service {
    constructor(c) { super(c, 'skills'); }
    register(input) {
      const skill = contribution(input);
      return this.ctx.effect(() => {
        if (names.has(skill.id)) return () => {};
        names.add(skill.id);
        const release = register(skill);
        return () => { names.delete(skill.id); return release(); };
      });
    }
  });
}
