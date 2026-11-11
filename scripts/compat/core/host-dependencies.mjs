// 安装期与运行期共用的宿主依赖边界。只有发行清单明确携带的运行库才重用；
// npm 作用域/包名前缀不等于“宿主已提供”，普通依赖仍由原包自己的依赖图解析。
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

export function hostDependencies(sharedRoot) {
  const pkg = JSON.parse(readFileSync(join(sharedRoot, 'package.json'), 'utf8'));
  return new Set(Object.keys(pkg.dependencies ?? {}).filter((name) =>
    name === '@deepseek-ai/cordis' || name === '@deepseek-ai/schemastery' || name.startsWith('@deepseek-ai/dsh-')
      || name === '@earendil-works/pi-ai' || name === 'react' || name === 'react-dom'));
}

export const packageName = (specifier) => specifier.startsWith('@')
  ? specifier.split('/').slice(0, 2).join('/') : specifier.split('/')[0];
