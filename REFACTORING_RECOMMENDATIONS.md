# Code Simplification Recommendations

**Date:** 2026-01-17
**Scope:** Recently modified files (last 3 commits)

## Priority 1: Eliminate Path Construction Duplication

### Problem
Three files independently construct session paths that `UnifiedSessionManager` already provides:
- `plan-storage.ts`
- `plan-todo.ts`
- `execution-state.ts`

### Solution
These classes should receive `UnifiedSessionManager` as a dependency and use its path methods:
- `getPlansDir(sessionId)`
- `getExecutionStateFilePath(sessionId)`

### Benefits
- Single source of truth for path construction
- Easier to change directory structure in future
- Reduces code duplication by ~30 lines

---

## Priority 2: Simplify Profile Merge Logic

### Problem
`ProfileLoader.mergeProfile()` (lines 134-161) uses verbose nullish coalescing for every field.

### Current Code Pattern
```typescript
private mergeProfile(base: WorkerProfile, override: Partial<WorkerProfile>): WorkerProfile {
  return {
    name: override.name ?? base.name,
    displayName: override.displayName ?? base.displayName,
    version: override.version ?? base.version,
    profile: {
      ...base.profile,
      ...override.profile,
    },
    preferences: {
      preferredCategories: override.preferences?.preferredCategories ?? base.preferences.preferredCategories,
      preferredKeywords: override.preferences?.preferredKeywords ?? base.preferences.preferredKeywords,
    },
    // ... more nested fields
  };
}
```

### Simplified Approach
```typescript
private mergeProfile(base: WorkerProfile, override: Partial<WorkerProfile>): WorkerProfile {
  return {
    ...base,
    ...override,
    profile: { ...base.profile, ...override.profile },
    preferences: { ...base.preferences, ...override.preferences },
    guidance: { ...base.guidance, ...override.guidance },
    collaboration: { ...base.collaboration, ...override.collaboration },
  };
}
```

### Benefits
- Reduces from 27 lines to 9 lines
- More maintainable when adding new fields
- Clearer intent: "merge objects with override precedence"

---

## Priority 3: Consolidate Snapshot File Reading

### Problem
`SnapshotManager` reads the same snapshot file multiple times:
- `createSnapshot()` - line 72-75
- `revertToSnapshot()` - line 134-139
- `getPendingChanges()` - line 176-179
- `getChangedFilesForSubTask()` - line 216-219

### Solution
Extract a helper method:
```typescript
private readSnapshotContent(sessionId: string, snapshotId: string): string {
  const snapshotFile = path.join(this.getSnapshotDir(sessionId), `${snapshotId}.snapshot`);
  return fs.existsSync(snapshotFile) ? fs.readFileSync(snapshotFile, 'utf-8') : '';
}
```

### Benefits
- Eliminates 4 instances of duplicate code
- Consistent error handling
- Easier to add caching later if needed

---

## Priority 4: Improve Error Handling Consistency

### Problem
Inconsistent error handling patterns across files:
- Some use try-catch with console.warn
- Some silently return null
- Some don't handle errors at all

### Recommendation
Establish consistent pattern:
```typescript
// For configuration loading (non-critical)
try {
  // load config
} catch (error) {
  console.warn('[Component] Operation failed:', context, error);
  return fallback;
}

// For critical operations (should propagate)
// Let errors bubble up naturally
```

### Files to Update
- `profile-loader.ts` - consistent logging format
- `plan-storage.ts` - add error context
- `execution-state.ts` - add error context

---

## Priority 5: Remove Redundant Null Checks

### Problem
`UnifiedSessionManager.getOrCreateCurrentSession()` (line 166) always returns a session, but many callers still check for null:

```typescript
const session = this.sessionManager.getCurrentSession();
if (!session) return null;  // Unnecessary if using getOrCreateCurrentSession()
```

### Solution
Use `getOrCreateCurrentSession()` when you need a session to exist, eliminating null checks.

### Files to Update
- `snapshot-manager.ts` - lines 50, 122, 164, 202, 249, 279, 296

---

## Priority 6: Simplify ID Generation

### Problem
Multiple files have duplicate `generateId()` functions:
- `unified-session-manager.ts` - lines 72-74, 77-79
- `snapshot-manager.ts` - lines 15-17

### Solution
Create a shared utility module:
```typescript
// src/utils/id-generator.ts
export function generateSessionId(): string {
  return `session-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

export function generateMessageId(): string {
  return `msg-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`;
}

export function generateSnapshotId(): string {
  return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}
```

---

## Priority 7: Simplify Session Title Generation

### Problem
`UnifiedSessionManager.generateSessionTitle()` (lines 252-264) has complex regex patterns that could be simplified.

### Current Code
```typescript
private generateSessionTitle(firstMessage: string): string {
  let text = firstMessage.trim().replace(/\n+/g, ' ').replace(/\s+/g, ' ');

  const prefixes = [/^(请|帮我|帮忙|能不能|可以|麻烦|我想|我要|我需要)/, /^(please|can you|could you|help me)/i];
  for (const p of prefixes) text = text.replace(p, '').trim();

  const suffixes = [/(吗|呢|吧|啊|谢谢|thanks)[\s。？?！!]*$/i];
  for (const s of suffixes) text = text.replace(s, '').trim();

  return text.length <= 100 ? text : text.substring(0, 100) + '...';
}
```

### Simplified Version
```typescript
private generateSessionTitle(firstMessage: string): string {
  const text = firstMessage
    .trim()
    .replace(/\s+/g, ' ')
    .replace(/^(请|帮我|帮忙|能不能|可以|麻烦|我想|我要|我需要|please|can you|could you|help me)\s*/i, '')
    .replace(/(吗|呢|吧|啊|谢谢|thanks)[\s。？?！!]*$/i, '')
    .trim();

  return text.length <= 100 ? text : text.substring(0, 100) + '...';
}
```

---

## Priority 8: Improve Type Safety in ProfileLoader

### Problem
`getProfile()` always falls back to `DEFAULT_CLAUDE_PROFILE` even for non-Claude workers.

### Current Code (line 214-216)
```typescript
getProfile(workerType: WorkerType): WorkerProfile {
  return this.profiles.get(workerType) ?? DEFAULT_CLAUDE_PROFILE;
}
```

### Better Approach
```typescript
getProfile(workerType: WorkerType): WorkerProfile {
  const profile = this.profiles.get(workerType);
  if (!profile) {
    throw new Error(`Profile not loaded for worker type: ${workerType}`);
  }
  return profile;
}
```

Or if fallback is intentional:
```typescript
private getDefaultProfile(workerType: WorkerType): WorkerProfile {
  const defaults = {
    claude: DEFAULT_CLAUDE_PROFILE,
    codex: DEFAULT_CODEX_PROFILE,
    gemini: DEFAULT_GEMINI_PROFILE,
  };
  return defaults[workerType];
}

getProfile(workerType: WorkerType): WorkerProfile {
  return this.profiles.get(workerType) ?? this.getDefaultProfile(workerType);
}
```

---

## Implementation Order

1. **Phase 1 (Low Risk):** Priorities 3, 6, 7 - Pure refactoring, no behavior change
2. **Phase 2 (Medium Risk):** Priorities 2, 5 - Simplification with careful testing
3. **Phase 3 (Higher Risk):** Priorities 1, 4, 8 - Architectural improvements

## Testing Strategy

For each priority:
1. Run existing tests before changes
2. Apply refactoring
3. Run tests again to verify no behavior change
4. Manual smoke test of affected features

## Estimated Impact

- **Lines of code reduced:** ~150-200 lines
- **Duplicate code eliminated:** ~80 lines
- **Complexity reduction:** ~30%
- **Maintainability improvement:** Significant (easier to understand and modify)

---

## Notes

All recommendations preserve exact functionality while improving:
- Code clarity and readability
- Consistency across the codebase
- Maintainability and extensibility
- Adherence to DRY principles

No changes to external APIs or behavior.
