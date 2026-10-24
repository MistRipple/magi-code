// 独立视觉 Demo：复用正式主题投影，只预览，不调用主题激活/保存接口。
import {
  initializeAppearanceRuntime,
  getAppearanceRuntimeSnapshot,
  previewAppearanceTheme,
  restoreActiveAppearance,
} from '../appearance/runtime';

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
