import assert from 'node:assert/strict';
import { waitFor } from './electron-harness.mjs';

// 公共页面验收只切换真实外观 API，不注入样式或修改原包组件。
export async function createPluginThemeCases(request) {
  const appearance = await request('/api/appearance/bootstrap');
  const cases = ['light', 'dark'].map(mode => ({ id: `builtin.${mode}`, mode }));
  const wallpaper = appearance.themes.find(theme => theme.pack.wallpaper)?.pack.wallpaper;
  assert.ok(wallpaper, '材质验收必须使用实际壁纸');
  for (const mode of ['light', 'dark']) for (const material of ['clear', 'translucent', 'immersive']) {
    const id = `acceptance.original-${mode}-${material}`;
    const scheme = mode === 'light' ? { accent: '#276749', background: '#f4ede3', foreground: '#29342c', contrast: 60 }
      : { accent: '#94bf85', background: '#17251f', foreground: '#e1efdf', contrast: 60 };
    const current = await request('/api/appearance/bootstrap');
    await request('/api/appearance/themes', { expectedRevision: current.revision, pack: { schemaVersion: 1, id,
      name: `Original UI ${mode} ${material}`, schemePolicy: mode, schemes: { [mode]: scheme }, material, wallpaper } });
    cases.push({ id, mode, material });
  }
  return { cases, original: appearance.activeThemeId };
}

export async function activatePluginTheme(request, page, frame, id) {
  const current = await request('/api/appearance/bootstrap');
  await request('/api/appearance/activate', { themeId: id, expectedRevision: current.revision });
  await waitFor(() => page.evaluate(`document.documentElement.dataset.magiThemeId===${JSON.stringify(id)}`), 'Magi 应用真实皮肤与材质');
  const names = ['--magi-canvas', '--success-text', '--warning-text', '--error-text', '--info-text'];
  const expression = `JSON.stringify(${JSON.stringify(names)}.map(name=>getComputedStyle(document.documentElement).getPropertyValue(name).trim()))`;
  const expected = await page.evaluate(expression);
  await waitFor(() => frame.evaluate(`${expression}===${JSON.stringify(expected)}`), '原包页面收到本次共享颜色，不能只核对深浅模式');
}

export async function measurePluginContrast(frame, selector) {
  return frame.evaluate(`(${measure.toString()})(${JSON.stringify(selector)})`);
}

function measure(selector) {
  const context = document.createElement('canvas').getContext('2d', { willReadFrequently: true });
  const pixel = color => {
    context.clearRect(0, 0, 1, 1); context.fillStyle = color; context.fillRect(0, 0, 1, 1);
    return [...context.getImageData(0, 0, 1, 1).data];
  };
  const luminance = rgb => rgb.slice(0, 3).map(value => value / 255)
    .map(value => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4)
    .reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
  return [...document.querySelectorAll(selector)].map(element => {
    const style = getComputedStyle(element), foreground = pixel(style.color), fill = pixel(style.backgroundColor);
    context.clearRect(0, 0, 1, 1);
    context.fillStyle = getComputedStyle(document.documentElement).getPropertyValue('--magi-canvas'); context.fillRect(0, 0, 1, 1);
    const chain = []; for (let node = element; node; node = node.parentElement) chain.unshift(node);
    for (const node of chain) { context.fillStyle = getComputedStyle(node).backgroundColor; context.fillRect(0, 0, 1, 1); }
    const background = [...context.getImageData(0, 0, 1, 1).data];
    context.fillStyle = style.color; context.fillRect(0, 0, 1, 1);
    const renderedForeground = [...context.getImageData(0, 0, 1, 1).data];
    const values = [luminance(renderedForeground), luminance(background)].sort((a, b) => a - b);
    return { kind: element.dataset.themeContrast ?? element.className, text: element.textContent.trim(), foreground, fill, background, ratio: (values[1] + 0.05) / (values[0] + 0.05) };
  });
}
