export function sanitizeMermaidSvgContent(svgContent: string): string {
  const trimmed = svgContent.trim();
  if (!trimmed || typeof document === 'undefined') {
    return '';
  }

  const template = document.createElement('template');
  template.innerHTML = trimmed;
  const svg = template.content.querySelector('svg');
  if (!svg) {
    return '';
  }

  svg.querySelectorAll('script').forEach((node) => node.remove());
  [svg, ...Array.from(svg.querySelectorAll('*'))].forEach((element) => {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      const value = attribute.value.trim().toLowerCase();
      if (name.startsWith('on') || value.startsWith('javascript:')) {
        element.removeAttribute(attribute.name);
      }
    }
  });

  return svg.outerHTML;
}
