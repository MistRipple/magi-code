import re

filepath = 'src/orchestrator/prompts/orchestrator-prompts.ts'

with open(filepath, 'r', encoding='utf-8') as f:
    content = f.read()

left_q = chr(0x201C)   # "
right_q = chr(0x201D)  # "

before_count = content.count(f'category: {left_q}')
print(f'发现 {before_count} 处弯引号 category 引用')

content = content.replace(
    f'category: {left_q}backend{right_q}',
    'ownership_hint: "backend", mode_hint: "implement"'
)
content = content.replace(
    f'category: {left_q}frontend{right_q}',
    'ownership_hint: "frontend", mode_hint: "implement"'
)
content = content.replace(
    f'category: {left_q}integration{right_q}',
    'ownership_hint: "integration", mode_hint: "implement"'
)

after_count = content.count(f'category: {left_q}')
print(f'修复后剩余 {after_count} 处弯引号 category 引用')

with open(filepath, 'w', encoding='utf-8') as f:
    f.write(content)

print('done')

