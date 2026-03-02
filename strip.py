import sys

with open('/Users/xie/code/magi/src/tools/file-executor.ts', 'r') as f:
    lines = f.readlines()

def find_brace_end(lines, start_idx):
    open_braces = 0
    in_block = False
    for i in range(start_idx, len(lines)):
        line = lines[i]
        if '{' in line:
            open_braces += line.count('{')
            in_block = True
        if '}' in line:
            open_braces -= line.count('}')
        
        if in_block and open_braces == 0:
            return i
    return -1

methods_to_remove = [
    'private getFileBulkEditDefinition(',
    'private extractEditEntries(',
    'private async executeBulkEdit(',
    'private async executeStrReplace(',
    'private buildEditSuccessMessage(',
    'private findOverlappingEntry(',
    'private matchAndReplace(',
    'private tryExactMatch(',
    'private tryWhitespaceNormalizedMatch(',
    'private rebaseSuccessfulEntries(',
    'private tryFuzzyMatch(',
    'private isFuzzyMatchFallbackEnabled',
    'private getNormalizedLines',
    'private convertIndent(',
    'private extractIndentInfo(',
    'private generateFuzzyProbeQuery',
    'private computeFuzzyMatchScore',
]

interfaces_to_remove = [
    'interface EditEntry {',
    'interface SuccessfulReplaceEntry {',
    'interface BulkEditFileEntry {',
    'interface InsertEntry {',
    'interface MatchLocation {',
    'interface IndentInfo {',
    'interface ReplaceResult {'
]

output_lines = []
skip_until = -1

for i, line in enumerate(lines):
    if i <= skip_until:
        continue
        
    should_skip = False
    
    # Check for interface removal
    for intf in interfaces_to_remove:
        if line.startswith(intf):
            end_idx = find_brace_end(lines, i)
            if end_idx != -1:
                # Also remove the doc comment right above it
                start_remove = i
                while start_remove > 0 and lines[start_remove-1].strip().startswith('*') or lines[start_remove-1].strip().startswith('/**'):
                    start_remove -= 1
                if start_remove < i:
                    for j in range(len(output_lines) - (i - start_remove), len(output_lines)):
                        output_lines.pop()
                skip_until = end_idx
                should_skip = True
                break

    if should_skip:
        continue

    # Check for method removal
    for method in methods_to_remove:
        if method in line:
            end_idx = find_brace_end(lines, i)
            if end_idx != -1:
                start_remove = i
                while start_remove > 0 and (lines[start_remove-1].strip().startswith('*') or lines[start_remove-1].strip().startswith('/**') or lines[start_remove-1].strip().startswith('//')):
                    start_remove -= 1
                
                # pop doc comments from output
                to_pop = i - start_remove
                while to_pop > 0 and len(output_lines) > 0:
                    output_lines.pop()
                    to_pop -= 1
                    
                skip_until = end_idx
                should_skip = True
                break

    if not should_skip:
        # Check for tool_manager arrays and imports that should be stripped
        if "this.getFileBulkEditDefinition()" in line:
            continue
        if "toolName === 'file_bulk_edit'" in line:
            line = line.replace(" || toolName === 'file_bulk_edit'", "")
            output_lines.append(line)
            continue
        if "if (toolCall.name === 'file_bulk_edit')" in line:
            # Skip the whole if block
            end_idx = find_brace_end(lines, i)
            if end_idx != -1:
                skip_until = end_idx
                # Also remove the comment above it
                if len(output_lines) > 0 and 'file_bulk_edit' in output_lines[-1]:
                    output_lines.pop()
                should_skip = True
                continue
                
        if not should_skip:
            output_lines.append(line)

with open('/Users/xie/code/magi/src/tools/file-executor.ts', 'w') as f:
    f.writelines(output_lines)
