// 独立视觉 Demo：复用正式主题投影，只预览，不调用主题激活/保存接口。
import './card-preview.css';
import { mount } from 'svelte';
import FileTypeIcon from '../components/FileTypeIcon.svelte';
import { icons, iconStyle, type IconName } from '../lib/icons';
import {
  initializeAppearanceRuntime,
  getAppearanceRuntimeSnapshot,
  previewAppearanceTheme,
  restoreActiveAppearance,
} from '../appearance/runtime';

const demoIcons: Record<string, IconName> = {
  spark: 'sparkles', check: 'check', chevron: 'chevron-right', copy: 'copy',
  code: 'code', file: 'file-text', search: 'search', terminal: 'terminal',
  list: 'list', branch: 'git-branch', shield: 'shield', alert: 'warning',
  loader: 'loader', image: 'image', agent: 'bot', diagram: 'layers',
  question: 'question', clock: 'clock', external: 'external-link', pause: 'pause',
};
// Static review markup consumes the same drawing contract as Icon.svelte.
for (const use of document.querySelectorAll<SVGUseElement>('svg use')) {
  const svg = use.closest('svg');
  if (!svg) continue;
  const id = use.getAttribute('href')?.slice(1) ?? '';
  const name = demoIcons[id];
  if (!name) continue;
  svg.classList.add('magi-icon', 'demo-icon');
  svg.dataset.icon = name;
  svg.setAttribute('viewBox', iconStyle.viewBox);
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');
  svg.style.fill = iconStyle.fill;
  svg.style.stroke = iconStyle.stroke;
  svg.style.strokeWidth = String(iconStyle.strokeWidth);
  svg.style.strokeLinecap = iconStyle.strokeLinecap;
  svg.style.strokeLinejoin = iconStyle.strokeLinejoin;
}

const definitions = document.querySelector('#demo-icon-definitions');
if (definitions) {
  definitions.innerHTML = Object.entries(demoIcons).map(([id, name]) =>
    `<symbol id="${id}" viewBox="${iconStyle.viewBox}"><g fill="${iconStyle.fill}" stroke="${iconStyle.stroke}" stroke-width="${iconStyle.strokeWidth}" stroke-linecap="${iconStyle.strokeLinecap}" stroke-linejoin="${iconStyle.strokeLinejoin}">${icons[name]}</g></symbol>`,
  ).join('');
}

for (const target of document.querySelectorAll<HTMLElement>('[data-file-icon]')) {
  mount(FileTypeIcon, { target, props: { path: target.dataset.fileIcon!, size: 14 } });
}

const selector = document.querySelector<HTMLSelectElement>('#skin-select')!;
const badge = document.querySelector<HTMLElement>('#skin-badge')!;
let sequence = 0;

async function load(): Promise<void> {
  await initializeAppearanceRuntime();
  const { library } = getAppearanceRuntimeSnapshot();
  if (!library) return;
  selector.replaceChildren(...library.themes.map(({ pack }) => new Option(pack.name, pack.id)));
  selector.value = library.activeThemeId;
  badge.textContent = `当前皮肤：${getAppearanceRuntimeSnapshot().activeTheme?.name || ''}`;

  selector.addEventListener('change', async () => {
    const pack = library.themes.find(({ pack }) => pack.id === selector.value)?.pack;
    if (!pack) return;
    const request = ++sequence;
    badge.textContent = '加载皮肤…';
    try {
      await previewAppearanceTheme(pack);
      if (request === sequence) badge.textContent = `预览：${pack.name}`;
    } catch {
      if (request === sequence) badge.textContent = '皮肤加载失败，请重试';
    }
  });
  document.querySelector('#restore-skin')?.addEventListener('click', async () => {
    ++sequence;
    await restoreActiveAppearance();
    selector.value = library.activeThemeId;
    badge.textContent = `当前皮肤：${getAppearanceRuntimeSnapshot().activeTheme?.name || ''}`;
  });
}

void load().catch(() => {
  badge.textContent = '皮肤加载失败，当前为中性预览';
  selector.replaceChildren(new Option('无法读取皮肤', ''));
  selector.disabled = true;
});
