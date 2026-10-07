import type {
  UserQuestionAnswerDto,
  UserQuestionDto,
} from './rust-backend-types';

/** 用户对一道题的草稿：选中的选项 label，以及“其他”是否被选中与输入的文字。 */
export interface UserQuestionDraft {
  selected: string[];
  otherActive: boolean;
  other: string;
}

export function emptyUserQuestionDrafts(questions: readonly UserQuestionDto[]): UserQuestionDraft[] {
  return questions.map(() => ({ selected: [], otherActive: false, other: '' }));
}

/** 点选一个预设选项：单选替换（并取消“其他”），多选切换。 */
export function toggleUserQuestionOption(
  question: UserQuestionDto,
  draft: UserQuestionDraft,
  label: string,
): UserQuestionDraft {
  if (!question.options.some((option) => option.label === label)) return draft;
  if (!question.multiSelect) {
    return { selected: [label], otherActive: false, other: draft.other };
  }
  const selected = draft.selected.includes(label)
    ? draft.selected.filter((item) => item !== label)
    : [...draft.selected, label];
  return { ...draft, selected };
}

/** 点选“其他”：单选时取代预设选项，多选时和预设选项并存；再点一次取消。 */
export function toggleUserQuestionOther(
  question: UserQuestionDto,
  draft: UserQuestionDraft,
): UserQuestionDraft {
  const otherActive = !draft.otherActive;
  if (!question.multiSelect && otherActive) {
    return { selected: [], otherActive, other: draft.other };
  }
  return { ...draft, otherActive };
}

export function setUserQuestionOtherText(draft: UserQuestionDraft, other: string): UserQuestionDraft {
  return { ...draft, other };
}

export function isUserQuestionAnswered(draft: UserQuestionDraft): boolean {
  return draft.selected.length > 0 || (draft.otherActive && draft.other.trim().length > 0);
}

export function areUserQuestionsAnswered(drafts: readonly UserQuestionDraft[]): boolean {
  return drafts.length > 0 && drafts.every(isUserQuestionAnswered);
}

/** 把草稿整理成提交给后端的回答：选中项按选项原顺序，“其他”只在被选中且非空时带上。 */
export function buildUserQuestionAnswers(
  questions: readonly UserQuestionDto[],
  drafts: readonly UserQuestionDraft[],
): UserQuestionAnswerDto[] {
  return questions.map((question, index) => {
    const draft = drafts[index] ?? { selected: [], otherActive: false, other: '' };
    const selected = question.options
      .map((option) => option.label)
      .filter((label) => draft.selected.includes(label));
    const other = draft.otherActive ? draft.other.trim() : '';
    return other ? { selected, other } : { selected };
  });
}

export interface UserQuestionResultItem {
  header: string;
  question: string;
  selected: string[];
  other: string;
}

export type UserQuestionResultSummary =
  | { status: 'answered'; items: UserQuestionResultItem[] }
  | { status: 'skipped' }
  | { status: 'awaiting'; questionId: string };

/** 解析 `ask_user_question` 的工具结果 / 进行中载荷，供对话流里的工具条目展示摘要。 */
export function parseUserQuestionResult(payload: unknown): UserQuestionResultSummary | null {
  let value: unknown = payload;
  if (typeof value === 'string') {
    try {
      value = JSON.parse(value);
    } catch {
      return null;
    }
  }
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  if (record.status === 'awaiting_user_input') {
    return { status: 'awaiting', questionId: typeof record.question_id === 'string' ? record.question_id : '' };
  }
  if (record.status === 'skipped') return { status: 'skipped' };
  if (record.status !== 'answered' || !Array.isArray(record.answers)) return null;
  const items = record.answers.flatMap((entry): UserQuestionResultItem[] => {
    if (!entry || typeof entry !== 'object') return [];
    const answer = entry as Record<string, unknown>;
    return [{
      header: typeof answer.header === 'string' ? answer.header : '',
      question: typeof answer.question === 'string' ? answer.question : '',
      selected: Array.isArray(answer.selected)
        ? answer.selected.filter((item): item is string => typeof item === 'string')
        : [],
      other: typeof answer.other === 'string' ? answer.other : '',
    }];
  });
  return { status: 'answered', items };
}
