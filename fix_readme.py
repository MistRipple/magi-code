import pathlib

p = pathlib.Path('README.md')
t = p.read_text()

replacements = [
    ('(e.g., GPT-4o-mini)', '(e.g., Claude Sonnet)'),
    ('Bug 修复、代码重构、测试补全', 'Bug 修复、问题排查、代码审查'),
    ('Svelte, TailwindCSS (Concept)', 'Svelte, TailwindCSS'),
    ('Claude 3.5 Sonnet 或 GPT-4o', 'Claude Sonnet 或 GPT-4o'),
    ('Ctrl+Shift+M` (Mac: `Cmd+Shift+M', 'Ctrl+Shift+A` (Mac: `Cmd+Shift+A'),
]

for old, new in replacements:
    if old in t:
        t = t.replace(old, new)
        print(f'REPLACED: {old[:50]}')
    else:
        print(f'NOT FOUND: {old[:50]}')

p.write_text(t)
print('File written.')

