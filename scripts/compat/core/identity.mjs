import { createHash } from 'node:crypto';
const hash = (value) => createHash('sha256').update(value).digest('hex');
const slug = (value) => String(value).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');

// 显示段不承担身份：摘要覆盖未归一化的完整来源。升级仍须核对原生态与包名。
export function packageId(ecosystem, name) {
  if (!/^[a-z][a-z0-9-]*$/.test(ecosystem) || typeof name !== 'string' || !name) throw new Error('来源身份无效');
  return `${ecosystem}.p-${slug(name).slice(0, 22)}-${hash(`${ecosystem}\0${name}`)}`;
}
export function runtimeId(kind, name) {
  if (typeof name !== 'string' || !name || name.length > 512) throw new Error('运行期贡献名称无效');
  return `${kind}-${slug(name).slice(0, 20)}-${hash(`${kind}\0${name}`).slice(0, 32)}`;
}
