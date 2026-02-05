import { MessageHub, SubTaskView } from '../src/orchestrator/core/message-hub';
import { MessageType, StandardMessage } from '../src/protocol/message-protocol';

// Mock logging to prevent clutter
const consoleLog = console.log;
// console.log = () => {}; 

async function runTest() {
    consoleLog('🚀 Starting UX E2E Data Contract Test...');
    const hub = new MessageHub();
    const messages: StandardMessage[] = [];

    // Subscribe to messages
    hub.on('unified:message', (msg) => {
        messages.push(msg);
    });

    // --- Scenario 1: Worker Execution & Summary ---
    consoleLog('\n🧪 Testing Worker Output & Summary...');
    
    // 1.1 Worker Output
    hub.workerOutput('claude', 'Processing file...');
    
    // 1.2 Worker Summary (New Feature)
    hub.workerSummary('claude', 'Analysis complete. 3 files modified.');

    // Verify 1.1
    const outputMsg = messages.find(m => m.source === 'worker' && m.type === MessageType.TEXT);
    if (!outputMsg) throw new Error('❌ Worker Output message not received');
    if (outputMsg.agent !== 'claude') throw new Error('❌ Worker Output agent mismatch');
    consoleLog('✅ Worker Output: OK');

    // Verify 1.2
    const summaryMsg = messages.find(m => m.source === 'worker' && m.type === MessageType.RESULT);
    if (!summaryMsg) throw new Error('❌ Worker Summary message not received');
    if (summaryMsg.agent !== 'claude') throw new Error('❌ Worker Summary agent mismatch');
    consoleLog('✅ Worker Summary: OK');


    // --- Scenario 2: SubTask Card Status (Stopped) ---
    consoleLog('\n🧪 Testing SubTask Card Status (Stopped)...');
    
    const stoppedTask: SubTaskView = {
        id: 'task-123',
        title: 'Refactor Auth',
        status: 'stopped', // The new status we added
        worker: 'gemini',
        summary: 'User interrupted'
    };

    hub.subTaskCard(stoppedTask);

    const cardMsg = messages.find(m => m.metadata?.subTaskId === 'task-123');
    if (!cardMsg) throw new Error('❌ SubTask Card message not received');
    
    const cardData = cardMsg.metadata?.subTaskCard as SubTaskView;
    if (cardData.status !== 'stopped') throw new Error(`❌ SubTask Card status mismatch. Expected "stopped", got "${cardData.status}"`);
    consoleLog('✅ SubTask Card (Stopped): OK');


    // --- Scenario 3: Orchestrator Response (Renaming Check) ---
    consoleLog('\n🧪 Testing Orchestrator Response...');
    
    hub.orchestratorMessage('I have a plan.');
    
    const orchMsg = messages.find(m => m.source === 'orchestrator' && m.type === MessageType.TEXT && !m.metadata?.isStatusMessage);
    if (!orchMsg) throw new Error('❌ Orchestrator Message not received');
    // We confirm backend sends TEXT. Frontend classifier logic was verified by unit test previously.
    console.log('✅ Orchestrator Response: OK');

    console.log('\n🎉 All UX Data Contracts Verified!');
    
    // Cleanup to allow process to exit
    hub.dispose();
}

runTest().catch(e => {
    console.error(e);
    process.exit(1);
});