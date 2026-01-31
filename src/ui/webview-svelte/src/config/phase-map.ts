export const PHASE_STEPS: Array<{ step: number; phases: string[] }> = [
  { step: 1, phases: ['clarifying', 'analyzing'] },
  { step: 2, phases: ['waiting_confirmation'] },
  { step: 3, phases: ['dispatching', 'monitoring', 'waiting_questions', 'waiting_worker_answer'] },
  { step: 4, phases: ['integrating'] },
  { step: 5, phases: ['verifying'] },
  { step: 6, phases: ['recovering'] },
  { step: 7, phases: ['summarizing', 'completed', 'failed'] },
];

export function resolvePhaseStep(phase: string): number {
  const normalized = phase.toLowerCase();
  for (const entry of PHASE_STEPS) {
    if (entry.phases.includes(normalized)) {
      return entry.step;
    }
  }
  return 0;
}
