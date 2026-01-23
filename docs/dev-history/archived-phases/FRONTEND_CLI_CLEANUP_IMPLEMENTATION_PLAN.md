# Frontend CLI Cleanup - Implementation Plan

## 📅 Project Info
- **Start Date**: 2025-01-22
- **Status**: 🔄 In Progress
- **Priority**: 🔴 HIGH

---

## 🎯 Goal

Clean up all CLI-related terminology in frontend code to match the backend's Agent/Worker/LLM architecture.

**Total Scope**: ~368 occurrences across 7 core files (excluding CSS and lib files)

---

## 📋 Implementation Stages

### Stage 1: Core State Management ✅
**Files**: `js/core/state.js` (16 occurrences)
**Goal**: Update state definitions and data structures
**Changes**:
- `selectedCli` → `selectedAgent`
- `cli` fields in state objects → `agent`
- Comments and variable names

**Success Criteria**: State management uses Agent terminology
**Status**: Not Started

---

### Stage 2: API Layer ✅
**Files**: `js/core/vscode-api.js` (4 occurrences)
**Goal**: Update VSCode API message types
**Changes**:
- Message type fields: `cli` → `agent`
- Function parameters and comments

**Success Criteria**: API layer matches backend message format
**Status**: Not Started

---

### Stage 3: Event Handlers ✅
**Files**: `js/ui/event-handlers.js` (60 occurrences)
**Goal**: Update event handling logic
**Changes**:
- Event data fields: `cli` → `agent`
- Handler function parameters
- UI state updates

**Success Criteria**: Events use Agent terminology
**Status**: Not Started

---

### Stage 4: Message Handler ✅
**Files**: `js/ui/message-handler.js` (69 occurrences)
**Goal**: Update message processing and display
**Changes**:
- Message object fields: `cli` → `agent`
- Worker question handling
- State management calls

**Success Criteria**: Message handling uses Agent terminology
**Status**: Not Started

---

### Stage 5: Message Renderer ✅
**Files**: `js/ui/message-renderer.js` (121 occurrences)
**Goal**: Update message rendering and display
**Changes**:
- Template variables: `cli` → `agent`
- CSS class names: `cli-*` → `agent-*`
- Display labels and text

**Success Criteria**: Rendered messages show Agent terminology
**Status**: Not Started

---

### Stage 6: Main Application ✅
**Files**: `js/main.js` (35 occurrences)
**Goal**: Update main application logic
**Changes**:
- Global state: `cli` → `agent`
- Message handling
- Initialization code

**Success Criteria**: Main app uses Agent terminology
**Status**: Not Started

---

### Stage 7: HTML Template ✅
**Files**: `index.html` (63 occurrences)
**Goal**: Update HTML structure and inline scripts
**Changes**:
- Element IDs and classes
- Data attributes
- Inline script variables

**Success Criteria**: HTML uses Agent terminology
**Status**: Not Started

---

### Stage 8: CSS Styling (Optional) ⚠️
**Files**: `styles/*.css` (92 occurrences)
**Goal**: Update CSS class names and selectors
**Changes**:
- `.cli-*` → `.agent-*`
- CSS comments

**Success Criteria**: Styles use Agent terminology
**Status**: Not Started
**Note**: Low priority, can be done last

---

## 🔍 Key Patterns to Replace

### JavaScript Object Fields
```javascript
// Before
{ cli: 'claude', ... }
message.cli
state.selectedCli
awaitingState.cli

// After
{ agent: 'claude', ... }
message.agent
state.selectedAgent
awaitingState.agent
```

### Variable Names
```javascript
// Before
selectedCli
targetCli
cliName

// After
selectedAgent
targetAgent
agentName
```

### CSS Classes
```css
/* Before */
.cli-badge
.cli-indicator
.cli-name

/* After */
.agent-badge
.agent-indicator
.agent-name
```

### Comments and Labels
```javascript
// Before
// CLI 选择器
"CLI: claude"

// After
// Agent 选择器
"Agent: claude"
```

---

## ⚠️ Critical Considerations

### 1. Backend Compatibility
- Backend already uses `agent` field in messages
- Frontend must match to avoid data mismatch
- Test message flow after each stage

### 2. State Persistence
- WebView state is saved/restored
- Must update state structure carefully
- Consider migration for existing saved states

### 3. CSS Class Changes
- Changing CSS classes affects styling
- Must update both CSS and JS together
- Test visual appearance

### 4. Testing Strategy
- Compile after each stage
- Manual testing of affected features
- Verify message flow works

---

## 📊 Progress Tracking

| Stage | File | Occurrences | Status | Time |
|-------|------|-------------|--------|------|
| 1 | state.js | 16 | ⏳ Not Started | 15 min |
| 2 | vscode-api.js | 4 | ⏳ Not Started | 10 min |
| 3 | event-handlers.js | 60 | ⏳ Not Started | 30 min |
| 4 | message-handler.js | 69 | ⏳ Not Started | 45 min |
| 5 | message-renderer.js | 121 | ⏳ Not Started | 1 hour |
| 6 | main.js | 35 | ⏳ Not Started | 30 min |
| 7 | index.html | 63 | ⏳ Not Started | 45 min |
| 8 | CSS files | 92 | ⏳ Not Started | 30 min |
| **Total** | **8 files** | **460** | **0%** | **~4.5 hours** |

---

## 🎯 Success Criteria

- [ ] All JavaScript files use `agent` instead of `cli`
- [ ] All HTML uses Agent terminology
- [ ] CSS classes updated (optional)
- [ ] Backend-frontend message format matches
- [ ] Manual testing passes
- [ ] No console errors
- [ ] Visual appearance correct

---

## 📝 Notes

- **Approach**: Systematic, file-by-file replacement
- **Testing**: After each stage
- **Rollback**: Git commit after each successful stage
- **Documentation**: Update this plan as we progress

---

**Created**: 2025-01-22
**Last Updated**: 2025-01-22
**Status**: 📋 Planning Complete, Ready to Execute
