# MultiCLI UI Tests

This directory contains unit tests for the UI components.

## Test Framework

- **Jest**: JavaScript testing framework
- **JSDOM**: DOM implementation for Node.js

## Running Tests

```bash
npm test
```

## Test Files

- `code-block-renderer.test.js` - Tests for code block rendering
- `thinking-renderer.test.js` - Tests for thinking block rendering
- `tool-call-renderer.test.js` - Tests for tool call rendering
- `keyboard-shortcuts.test.js` - Tests for keyboard shortcuts
- `search-manager.test.js` - Tests for search functionality
- `performance.test.js` - Tests for performance utilities

## Writing Tests

Follow this pattern:

```javascript
import { renderCodeBlock } from '../src/ui/webview/js/ui/renderers/code-block-renderer.js';

describe('Code Block Renderer', () => {
  test('should render basic code block', () => {
    const html = renderCodeBlock({
      code: 'console.log("Hello");',
      language: 'javascript'
    });

    expect(html).toContain('c-code-block');
    expect(html).toContain('JavaScript');
    expect(html).toContain('console.log');
  });
});
```

## Coverage

Run coverage report:

```bash
npm run test:coverage
```

Target: 80% coverage for all components
