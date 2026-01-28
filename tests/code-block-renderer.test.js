/**
 * Code Block Renderer 单元测试
 */

import { renderCodeBlock, renderInlineCode, getLanguageName, generateId } from '../src/ui/webview/js/ui/renderers/code-block-renderer.js';

describe('Code Block Renderer', () => {
  describe('renderCodeBlock', () => {
    test('should render basic code block', () => {
      const html = renderCodeBlock({
        code: 'console.log("Hello World");',
        language: 'javascript'
      });

      expect(html).toContain('c-code-block');
      expect(html).toContain('JavaScript');
      expect(html).toContain('console.log');
    });

    test('should escape HTML in code', () => {
      const html = renderCodeBlock({
        code: '<script>alert("XSS")</script>',
        language: 'html'
      });

      expect(html).not.toContain('<script>');
      expect(html).toContain('&lt;script&gt;');
    });

    test('should render with file path', () => {
      const html = renderCodeBlock({
        code: 'const x = 1;',
        language: 'javascript',
        filepath: 'src/index.js'
      });

      expect(html).toContain('src/index.js');
      expect(html).toContain('c-code-block__filepath');
    });

    test('should show copy button by default', () => {
      const html = renderCodeBlock({
        code: 'test',
        language: 'text'
      });

      expect(html).toContain('copyCodeBlock');
      expect(html).toContain('复制');
    });

    test('should hide copy button when disabled', () => {
      const html = renderCodeBlock({
        code: 'test',
        language: 'text',
        showCopyButton: false
      });

      expect(html).not.toContain('copyCodeBlock');
    });

    test('should show apply button when filepath provided and enabled', () => {
      const html = renderCodeBlock({
        code: 'test',
        language: 'text',
        filepath: 'test.txt',
        showApplyButton: true
      });

      expect(html).toContain('applyCodeBlock');
      expect(html).toContain('应用');
    });

    test('should be collapsible for long code (>15 lines)', () => {
      const longCode = Array(20).fill('line').join('\n');
      const html = renderCodeBlock({
        code: longCode,
        language: 'text',
        maxHeight: 400
      });

      expect(html).toContain('c-code-block--collapsed');
      expect(html).toContain('toggleCodeBlock');
      expect(html).toContain('展开全部');
    });

    test('should not be collapsible for short code', () => {
      const shortCode = Array(10).fill('line').join('\n');
      const html = renderCodeBlock({
        code: shortCode,
        language: 'text',
        maxHeight: 400
      });

      expect(html).not.toContain('c-code-block--collapsed');
      expect(html).not.toContain('toggleCodeBlock');
    });

    test('should render with line numbers when enabled', () => {
      const html = renderCodeBlock({
        code: 'line1\nline2\nline3',
        language: 'text',
        showLineNumbers: true
      });

      expect(html).toContain('c-code-block--with-line-numbers');
      expect(html).toContain('c-code-block__line-number');
    });

    test('should use custom block ID if provided', () => {
      const customId = 'custom-block-id';
      const html = renderCodeBlock({
        code: 'test',
        language: 'text',
        blockId: customId
      });

      expect(html).toContain(`data-code-id="${customId}"`);
    });

    test('should generate unique ID if not provided', () => {
      const html1 = renderCodeBlock({ code: 'test1', language: 'text' });
      const html2 = renderCodeBlock({ code: 'test2', language: 'text' });

      // Extract IDs from HTML
      const id1Match = html1.match(/data-code-id="([^"]+)"/);
      const id2Match = html2.match(/data-code-id="([^"]+)"/);

      expect(id1Match).not.toBeNull();
      expect(id2Match).not.toBeNull();
      expect(id1Match[1]).not.toBe(id2Match[1]);
    });
  });

  describe('renderInlineCode', () => {
    test('should render inline code', () => {
      const html = renderInlineCode('const x = 1');

      expect(html).toContain('c-code-inline');
      expect(html).toContain('const x = 1');
    });

    test('should escape HTML in inline code', () => {
      const html = renderInlineCode('<div>');

      expect(html).not.toContain('<div>');
      expect(html).toContain('&lt;div&gt;');
    });

    test('should return empty string for empty input', () => {
      expect(renderInlineCode('')).toBe('');
      expect(renderInlineCode(null)).toBe('');
      expect(renderInlineCode(undefined)).toBe('');
    });
  });

  describe('getLanguageName', () => {
    test('should return correct display names', () => {
      expect(getLanguageName('js')).toBe('JavaScript');
      expect(getLanguageName('javascript')).toBe('JavaScript');
      expect(getLanguageName('ts')).toBe('TypeScript');
      expect(getLanguageName('py')).toBe('Python');
      expect(getLanguageName('python')).toBe('Python');
    });

    test('should handle unknown languages', () => {
      expect(getLanguageName('unknown')).toBe('UNKNOWN');
    });

    test('should handle empty/null input', () => {
      expect(getLanguageName('')).toBe('Code');
      expect(getLanguageName(null)).toBe('Code');
      expect(getLanguageName(undefined)).toBe('Code');
    });

    test('should be case insensitive', () => {
      expect(getLanguageName('JavaScript')).toBe('JavaScript');
      expect(getLanguageName('PYTHON')).toBe('Python');
    });
  });

  describe('generateId', () => {
    test('should generate unique IDs', () => {
      const id1 = generateId();
      const id2 = generateId();

      expect(id1).not.toBe(id2);
    });

    test('should start with "code-" prefix', () => {
      const id = generateId();

      expect(id).toMatch(/^code-/);
    });

    test('should contain timestamp and random component', () => {
      const id = generateId();
      const parts = id.split('-');

      expect(parts.length).toBeGreaterThanOrEqual(3);
      expect(parts[0]).toBe('code');
    });
  });
});
