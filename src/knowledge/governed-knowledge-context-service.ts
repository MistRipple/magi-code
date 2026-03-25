import { createHash } from 'crypto';
import { logger, LogCategory } from '../logging';
import { estimateMaxCharsForTokens, estimateTokenCount } from '../utils/token-estimator';
import type {
  ProjectKnowledgeBase,
  CodeIndex,
  ADRRecord,
  FAQRecord,
  LearningRecord,
  ADRStatus,
} from './project-knowledge-base';
import {
  FileKnowledgeGovernanceAuditStore,
  type KnowledgeGovernanceAuditRecord,
} from './knowledge-governance-audit-store';

export type GovernedKnowledgePurpose =
  | 'project_context'
  | 'knowledge_index'
  | 'tool_query'
  | 'knowledge_api'
  | 'ui_panel';

export interface GovernedKnowledgeAuditMetadata {
  purpose?: GovernedKnowledgePurpose;
  consumer?: string;
  sessionId?: string;
  requestId?: string;
  missionId?: string;
  assignmentId?: string;
  todoId?: string;
  agentId?: string;
  workerId?: string;
}

export type GovernedKnowledgeReferenceCategory = 'code_index' | 'adr' | 'faq' | 'learning';

export interface GovernedKnowledgeReference {
  category: GovernedKnowledgeReferenceCategory;
  id: string;
  title: string;
  status?: ADRStatus;
  relatedFiles?: string[];
  updatedAt?: string;
}

export interface GovernedKnowledgeMetadata {
  version: string;
  generatedAt: string;
  sourceUpdatedAt?: string;
  auditPath: string;
  counts: {
    files: number;
    adrs: number;
    acceptedAdrs: number;
    faqs: number;
    learnings: number;
  };
  references: GovernedKnowledgeReference[];
  audit: Required<Pick<GovernedKnowledgeAuditMetadata, 'purpose'>> & Omit<GovernedKnowledgeAuditMetadata, 'purpose'>;
}

export interface GovernedKnowledgeTextResult {
  metadata: GovernedKnowledgeMetadata;
  content: string;
}

export interface GovernedKnowledgeSnapshotResult {
  governance: GovernedKnowledgeMetadata;
  codeIndex: CodeIndex | null;
  adrs: ADRRecord[];
  faqs: FAQRecord[];
  learnings: LearningRecord[];
}

interface KnowledgeSourceSnapshot {
  codeIndex: CodeIndex | null;
  adrs: ADRRecord[];
  faqs: FAQRecord[];
  learnings: LearningRecord[];
}

export interface GovernedKnowledgeQueryInput {
  category?: 'adr' | 'faq' | 'learning' | 'all';
  id?: string;
  query?: string;
}

export class GovernedKnowledgeContextService {
  private static readonly HEADER_REFERENCE_LIMIT = 6;
  private static readonly INDEX_REFERENCE_LIMIT = 20;
  private readonly auditStore: FileKnowledgeGovernanceAuditStore;

  constructor(private readonly knowledgeBase: ProjectKnowledgeBase) {
    this.auditStore = new FileKnowledgeGovernanceAuditStore(this.knowledgeBase.getProjectRoot());
  }

  buildProjectContext(
    maxTokens: number = 800,
    audit: GovernedKnowledgeAuditMetadata = {},
  ): GovernedKnowledgeTextResult {
    const snapshot = this.captureSnapshot();
    const rawContent = this.knowledgeBase.getProjectContext(this.resolveContentBudget(maxTokens));
    const references = this.collectProjectContextReferences(snapshot);
    return this.finalizeTextResult(snapshot, rawContent, references, audit, 'project_context', maxTokens);
  }

  buildKnowledgeIndex(
    maxTokens: number = 600,
    audit: GovernedKnowledgeAuditMetadata = {},
  ): GovernedKnowledgeTextResult {
    const snapshot = this.captureSnapshot();
    const rawContent = this.knowledgeBase.getKnowledgeIndex(this.resolveContentBudget(maxTokens));
    const references = this.collectKnowledgeIndexReferences(snapshot);
    return this.finalizeTextResult(snapshot, rawContent, references, audit, 'knowledge_index', maxTokens);
  }

  buildQueryResult(
    input: GovernedKnowledgeQueryInput,
    audit: GovernedKnowledgeAuditMetadata = {},
  ): GovernedKnowledgeTextResult {
    const snapshot = this.captureSnapshot();
    const category = input.category || 'all';
    const id = this.normalizeString(input.id);
    const query = this.normalizeString(input.query)?.toLowerCase();

    if (id) {
      const record = this.findRecordById(snapshot, id);
      if (!record) {
        return this.finalizeTextResult(
          snapshot,
          `No record found with ID: ${id}`,
          [],
          audit,
          'tool_query',
        );
      }
      return this.finalizeTextResult(
        snapshot,
        this.formatRecordDetail(record),
        [record.reference],
        audit,
        'tool_query',
      );
    }

    switch (category) {
      case 'adr':
        return this.buildAdrQueryResult(snapshot, query, audit);
      case 'faq':
        return this.buildFaqQueryResult(snapshot, query, audit);
      case 'learning':
        return this.buildLearningQueryResult(snapshot, query, audit);
      case 'all':
        return this.buildAllQueryResult(snapshot, query, audit);
      default:
        return this.finalizeTextResult(
          snapshot,
          `Unknown category: ${category}. Use one of: adr, faq, learning, all`,
          [],
          audit,
          'tool_query',
        );
    }
  }

  buildKnowledgeSnapshot(
    audit: GovernedKnowledgeAuditMetadata = {},
  ): GovernedKnowledgeSnapshotResult {
    const snapshot = this.captureSnapshot();
    const references = this.collectAllReferences(snapshot);
    const governance = this.buildMetadata(snapshot, references, audit, 'knowledge_api');
    this.recordAudit(governance, 'snapshot');
    return {
      governance,
      codeIndex: snapshot.codeIndex,
      adrs: snapshot.adrs,
      faqs: snapshot.faqs,
      learnings: snapshot.learnings,
    };
  }

  buildAdrPayload(
    filter?: { status?: ADRStatus },
    audit: GovernedKnowledgeAuditMetadata = {},
  ): { governance: GovernedKnowledgeMetadata; adrs: ADRRecord[] } {
    const snapshot = this.captureSnapshot();
    const adrs = snapshot.adrs.filter((adr) => !filter?.status || adr.status === filter.status);
    const references = adrs.map((adr) => this.createAdrReference(adr));
    const governance = this.buildMetadata(snapshot, references, audit, 'knowledge_api');
    this.recordAudit(governance, 'adr_payload');
    return { governance, adrs };
  }

  buildFaqPayload(
    filter?: { category?: string },
    audit: GovernedKnowledgeAuditMetadata = {},
  ): { governance: GovernedKnowledgeMetadata; faqs: FAQRecord[] } {
    const snapshot = this.captureSnapshot();
    const faqs = snapshot.faqs.filter((faq) => !filter?.category || faq.category === filter.category);
    const references = faqs.map((faq) => this.createFaqReference(faq));
    const governance = this.buildMetadata(snapshot, references, audit, 'knowledge_api');
    this.recordAudit(governance, 'faq_payload');
    return { governance, faqs };
  }

  buildFaqSearchPayload(
    keyword: string,
    audit: GovernedKnowledgeAuditMetadata = {},
  ): { governance: GovernedKnowledgeMetadata; results: FAQRecord[] } {
    const normalizedKeyword = keyword.trim();
    const results = this.knowledgeBase.searchFAQs(normalizedKeyword);
    const snapshot = this.captureSnapshot();
    const references = results.map((faq) => this.createFaqReference(faq));
    const governance = this.buildMetadata(snapshot, references, audit, 'knowledge_api');
    this.recordAudit(governance, 'faq_search');
    return { governance, results };
  }

  private buildAdrQueryResult(
    snapshot: KnowledgeSourceSnapshot,
    query: string | undefined,
    audit: GovernedKnowledgeAuditMetadata,
  ): GovernedKnowledgeTextResult {
    let adrs = snapshot.adrs.filter((adr) => adr.status === 'accepted');
    if (query) {
      adrs = adrs.filter((adr) =>
        adr.title.toLowerCase().includes(query) ||
        adr.decision.toLowerCase().includes(query) ||
        adr.context.toLowerCase().includes(query),
      );
    }

    if (adrs.length === 0) {
      return this.finalizeTextResult(
        snapshot,
        query
          ? `No ADRs match query "${query}".`
          : 'No accepted ADRs found in the project knowledge base.',
        [],
        audit,
        'tool_query',
      );
    }

    const content = `# Architecture Decision Records (${adrs.length} found)\n\n${
      adrs
        .map((adr) => (
          `### ${adr.id}: ${adr.title}\n` +
          `**Status**: ${adr.status}\n` +
          `**Decision**: ${adr.decision}\n` +
          `**Context**: ${adr.context}`
        ))
        .join('\n\n---\n\n')
    }`;

    return this.finalizeTextResult(
      snapshot,
      content,
      adrs.map((adr) => this.createAdrReference(adr)),
      audit,
      'tool_query',
    );
  }

  private buildFaqQueryResult(
    snapshot: KnowledgeSourceSnapshot,
    query: string | undefined,
    audit: GovernedKnowledgeAuditMetadata,
  ): GovernedKnowledgeTextResult {
    let faqs = snapshot.faqs;
    if (query) {
      faqs = faqs.filter((faq) =>
        faq.question.toLowerCase().includes(query) ||
        faq.answer.toLowerCase().includes(query) ||
        faq.tags.some((tag) => tag.toLowerCase().includes(query)),
      );
    }

    if (faqs.length === 0) {
      return this.finalizeTextResult(
        snapshot,
        query
          ? `No FAQs match query "${query}".`
          : 'No FAQs found in the project knowledge base.',
        [],
        audit,
        'tool_query',
      );
    }

    const content = `# FAQs (${faqs.length} found)\n\n${
      faqs.map((faq) => `**Q**: ${faq.question}\n**A**: ${faq.answer}`).join('\n\n---\n\n')
    }`;

    return this.finalizeTextResult(
      snapshot,
      content,
      faqs.map((faq) => this.createFaqReference(faq)),
      audit,
      'tool_query',
    );
  }

  private buildLearningQueryResult(
    snapshot: KnowledgeSourceSnapshot,
    query: string | undefined,
    audit: GovernedKnowledgeAuditMetadata,
  ): GovernedKnowledgeTextResult {
    let learnings = snapshot.learnings;
    if (query) {
      learnings = learnings.filter((learning) =>
        learning.content.toLowerCase().includes(query) ||
        learning.context.toLowerCase().includes(query),
      );
    }

    if (learnings.length === 0) {
      return this.finalizeTextResult(
        snapshot,
        query
          ? `No learnings match query "${query}".`
          : 'No learnings found in the project knowledge base.',
        [],
        audit,
        'tool_query',
      );
    }

    const content = `# Past Learnings (${learnings.length} found)\n\n${
      learnings
        .map((learning) => `### ${learning.id}\n**Context**: ${learning.context}\n**Insight**: ${learning.content}`)
        .join('\n\n---\n\n')
    }`;

    return this.finalizeTextResult(
      snapshot,
      content,
      learnings.map((learning) => this.createLearningReference(learning)),
      audit,
      'tool_query',
    );
  }

  private buildAllQueryResult(
    snapshot: KnowledgeSourceSnapshot,
    query: string | undefined,
    audit: GovernedKnowledgeAuditMetadata,
  ): GovernedKnowledgeTextResult {
    const sections: string[] = [];
    const references: GovernedKnowledgeReference[] = [];

    const acceptedAdrs = snapshot.adrs.filter((adr) => adr.status === 'accepted');
    const filteredAdrs = query
      ? acceptedAdrs.filter((adr) =>
        adr.title.toLowerCase().includes(query) ||
        adr.decision.toLowerCase().includes(query),
      )
      : acceptedAdrs;
    if (filteredAdrs.length > 0) {
      sections.push(`## ADRs (${filteredAdrs.length})\n${filteredAdrs.map((adr) => `- [${adr.id}] ${adr.title}`).join('\n')}`);
      references.push(...filteredAdrs.map((adr) => this.createAdrReference(adr)));
    }

    const filteredFaqs = query
      ? snapshot.faqs.filter((faq) =>
        faq.question.toLowerCase().includes(query) ||
        faq.tags.some((tag) => tag.toLowerCase().includes(query)),
      )
      : snapshot.faqs;
    if (filteredFaqs.length > 0) {
      sections.push(`## FAQs (${filteredFaqs.length})\n${filteredFaqs.map((faq) => `- [${faq.id}] ${faq.question}`).join('\n')}`);
      references.push(...filteredFaqs.map((faq) => this.createFaqReference(faq)));
    }

    const filteredLearnings = query
      ? snapshot.learnings.filter((learning) => learning.content.toLowerCase().includes(query))
      : snapshot.learnings;
    if (filteredLearnings.length > 0) {
      sections.push(
        `## Learnings (${filteredLearnings.length})\n${
          filteredLearnings.map((learning) => `- [${learning.id}] ${learning.content.substring(0, 80)}`).join('\n')
        }`,
      );
      references.push(...filteredLearnings.map((learning) => this.createLearningReference(learning)));
    }

    if (sections.length === 0) {
      return this.finalizeTextResult(
        snapshot,
        query
          ? `No knowledge base entries match query "${query}".`
          : 'Project knowledge base is empty.',
        [],
        audit,
        'tool_query',
      );
    }

    return this.finalizeTextResult(
      snapshot,
      `# Project Knowledge Base Index\n\nUse project_knowledge_query with a specific ID to get full details.\n\n${sections.join('\n\n')}`,
      references,
      audit,
      'tool_query',
    );
  }

  private finalizeTextResult(
    snapshot: KnowledgeSourceSnapshot,
    rawContent: string,
    references: GovernedKnowledgeReference[],
    audit: GovernedKnowledgeAuditMetadata,
    defaultPurpose: GovernedKnowledgePurpose,
    maxTokens?: number,
  ): GovernedKnowledgeTextResult {
    const metadata = this.buildMetadata(snapshot, references, audit, defaultPurpose);
    const content = rawContent.trim().length > 0
      ? this.maybeTrimToTokens(
        `${this.renderGovernanceHeader(metadata)}\n${rawContent}`,
        maxTokens,
      )
      : rawContent;
    this.recordAudit(metadata, 'text_result');
    return { metadata, content };
  }

  private buildMetadata(
    snapshot: KnowledgeSourceSnapshot,
    references: GovernedKnowledgeReference[],
    audit: GovernedKnowledgeAuditMetadata,
    defaultPurpose: GovernedKnowledgePurpose,
  ): GovernedKnowledgeMetadata {
    const generatedAt = new Date().toISOString();
    const sourceUpdatedAt = this.computeSourceUpdatedAt(snapshot);
    return {
      version: this.computeVersion(snapshot),
      generatedAt,
      sourceUpdatedAt,
      auditPath: this.auditStore.getStoragePath(),
      counts: {
        files: snapshot.codeIndex?.files.length ?? 0,
        adrs: snapshot.adrs.length,
        acceptedAdrs: snapshot.adrs.filter((adr) => adr.status === 'accepted').length,
        faqs: snapshot.faqs.length,
        learnings: snapshot.learnings.length,
      },
      references,
      audit: {
        purpose: audit.purpose || defaultPurpose,
        ...(this.normalizeOptionalField(audit.consumer) ? { consumer: this.normalizeOptionalField(audit.consumer)! } : {}),
        ...(this.normalizeOptionalField(audit.sessionId) ? { sessionId: this.normalizeOptionalField(audit.sessionId)! } : {}),
        ...(this.normalizeOptionalField(audit.requestId) ? { requestId: this.normalizeOptionalField(audit.requestId)! } : {}),
        ...(this.normalizeOptionalField(audit.missionId) ? { missionId: this.normalizeOptionalField(audit.missionId)! } : {}),
        ...(this.normalizeOptionalField(audit.assignmentId) ? { assignmentId: this.normalizeOptionalField(audit.assignmentId)! } : {}),
        ...(this.normalizeOptionalField(audit.todoId) ? { todoId: this.normalizeOptionalField(audit.todoId)! } : {}),
        ...(this.normalizeOptionalField(audit.agentId) ? { agentId: this.normalizeOptionalField(audit.agentId)! } : {}),
        ...(this.normalizeOptionalField(audit.workerId) ? { workerId: this.normalizeOptionalField(audit.workerId)! } : {}),
      },
    };
  }

  private recordAudit(metadata: GovernedKnowledgeMetadata, resultKind: string): void {
    const record: KnowledgeGovernanceAuditRecord = {
      schemaVersion: 'knowledge-governance-audit.v1',
      eventId: `knowledge_audit_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
      timestamp: Date.now(),
      generatedAt: metadata.generatedAt,
      resultKind,
      version: metadata.version,
      sourceUpdatedAt: metadata.sourceUpdatedAt,
      purpose: metadata.audit.purpose,
      consumer: metadata.audit.consumer,
      sessionId: metadata.audit.sessionId,
      requestId: metadata.audit.requestId,
      missionId: metadata.audit.missionId,
      assignmentId: metadata.audit.assignmentId,
      todoId: metadata.audit.todoId,
      agentId: metadata.audit.agentId,
      workerId: metadata.audit.workerId,
      referenceCount: metadata.references.length,
      references: metadata.references,
    };
    this.auditStore.append(record);

    logger.info('知识治理.上下文访问', {
      resultKind,
      purpose: metadata.audit.purpose,
      consumer: metadata.audit.consumer,
      version: metadata.version,
      generatedAt: metadata.generatedAt,
      sourceUpdatedAt: metadata.sourceUpdatedAt,
      sessionId: metadata.audit.sessionId,
      requestId: metadata.audit.requestId,
      missionId: metadata.audit.missionId,
      assignmentId: metadata.audit.assignmentId,
      todoId: metadata.audit.todoId,
      agentId: metadata.audit.agentId,
      workerId: metadata.audit.workerId,
      referenceCount: metadata.references.length,
      auditPath: metadata.auditPath,
    }, LogCategory.SESSION);
  }

  private renderGovernanceHeader(metadata: GovernedKnowledgeMetadata): string {
    const referenceSummary = this.formatReferenceSummary(metadata.references);
    const segments = [
      `version=${metadata.version}`,
      `generatedAt=${metadata.generatedAt}`,
      `sourceUpdatedAt=${metadata.sourceUpdatedAt || 'n/a'}`,
      `refs=${referenceSummary || 'none'}`,
    ];
    return `[Knowledge Snapshot | ${segments.join(' | ')}]`;
  }

  private formatReferenceSummary(references: GovernedKnowledgeReference[]): string {
    if (references.length === 0) {
      return '';
    }
    const unique = new Map<string, GovernedKnowledgeReference>();
    for (const reference of references) {
      const key = `${reference.category}:${reference.id}`;
      if (!unique.has(key)) {
        unique.set(key, reference);
      }
    }
    const visible = Array.from(unique.values()).slice(0, GovernedKnowledgeContextService.HEADER_REFERENCE_LIMIT);
    const suffix = unique.size > visible.length ? ` +${unique.size - visible.length}` : '';
    return `${visible.map((reference) => `${reference.category}:${reference.id}`).join(', ')}${suffix}`;
  }

  private collectProjectContextReferences(snapshot: KnowledgeSourceSnapshot): GovernedKnowledgeReference[] {
    const references: GovernedKnowledgeReference[] = [];
    if (snapshot.codeIndex) {
      references.push(this.createCodeIndexReference(snapshot.codeIndex));
    }
    references.push(
      ...snapshot.adrs
        .filter((adr) => adr.status === 'accepted')
        .slice(0, 3)
        .map((adr) => this.createAdrReference(adr)),
    );
    references.push(
      ...[...snapshot.faqs]
        .sort((left, right) => right.useCount - left.useCount)
        .slice(0, 2)
        .map((faq) => this.createFaqReference(faq)),
    );
    return references;
  }

  private collectKnowledgeIndexReferences(snapshot: KnowledgeSourceSnapshot): GovernedKnowledgeReference[] {
    const references: GovernedKnowledgeReference[] = [];
    references.push(
      ...snapshot.adrs
        .filter((adr) => adr.status === 'accepted')
        .slice(0, GovernedKnowledgeContextService.INDEX_REFERENCE_LIMIT)
        .map((adr) => this.createAdrReference(adr)),
    );
    references.push(
      ...snapshot.faqs
        .slice(0, GovernedKnowledgeContextService.INDEX_REFERENCE_LIMIT)
        .map((faq) => this.createFaqReference(faq)),
    );
    references.push(
      ...snapshot.learnings
        .slice(0, GovernedKnowledgeContextService.INDEX_REFERENCE_LIMIT)
        .map((learning) => this.createLearningReference(learning)),
    );
    return references;
  }

  private collectAllReferences(snapshot: KnowledgeSourceSnapshot): GovernedKnowledgeReference[] {
    const references: GovernedKnowledgeReference[] = [];
    if (snapshot.codeIndex) {
      references.push(this.createCodeIndexReference(snapshot.codeIndex));
    }
    references.push(...snapshot.adrs.map((adr) => this.createAdrReference(adr)));
    references.push(...snapshot.faqs.map((faq) => this.createFaqReference(faq)));
    references.push(...snapshot.learnings.map((learning) => this.createLearningReference(learning)));
    return references;
  }

  private createCodeIndexReference(codeIndex: CodeIndex): GovernedKnowledgeReference {
    return {
      category: 'code_index',
      id: 'project-code-index',
      title: `${codeIndex.files.length} indexed files`,
      updatedAt: new Date(codeIndex.lastIndexed).toISOString(),
    };
  }

  private createAdrReference(adr: ADRRecord): GovernedKnowledgeReference {
    return {
      category: 'adr',
      id: adr.id,
      title: adr.title,
      status: adr.status,
      relatedFiles: adr.relatedFiles,
      updatedAt: new Date(adr.date).toISOString(),
    };
  }

  private createFaqReference(faq: FAQRecord): GovernedKnowledgeReference {
    return {
      category: 'faq',
      id: faq.id,
      title: faq.question,
      relatedFiles: faq.relatedFiles,
      updatedAt: new Date(Math.max(faq.updatedAt, faq.createdAt)).toISOString(),
    };
  }

  private createLearningReference(learning: LearningRecord): GovernedKnowledgeReference {
    return {
      category: 'learning',
      id: learning.id,
      title: learning.content.substring(0, 80),
      updatedAt: new Date(learning.createdAt).toISOString(),
    };
  }

  private findRecordById(
    snapshot: KnowledgeSourceSnapshot,
    id: string,
  ): { reference: GovernedKnowledgeReference; record: ADRRecord | FAQRecord | LearningRecord; kind: 'adr' | 'faq' | 'learning' } | null {
    const adr = snapshot.adrs.find((item) => item.id === id);
    if (adr) {
      return { reference: this.createAdrReference(adr), record: adr, kind: 'adr' };
    }

    const faq = snapshot.faqs.find((item) => item.id === id);
    if (faq) {
      return { reference: this.createFaqReference(faq), record: faq, kind: 'faq' };
    }

    const learning = snapshot.learnings.find((item) => item.id === id);
    if (learning) {
      return { reference: this.createLearningReference(learning), record: learning, kind: 'learning' };
    }

    return null;
  }

  private formatRecordDetail(record: {
    record: ADRRecord | FAQRecord | LearningRecord;
    kind: 'adr' | 'faq' | 'learning';
  }): string {
    if (record.kind === 'adr') {
      const adr = record.record as ADRRecord;
      const parts = [
        `# ADR: ${adr.title}`,
        `**ID**: ${adr.id}`,
        `**Status**: ${adr.status}`,
        `**Date**: ${new Date(adr.date).toISOString().split('T')[0]}`,
        '',
        `## Context\n${adr.context}`,
        `## Decision\n${adr.decision}`,
        `## Consequences\n${adr.consequences}`,
      ];
      if (adr.alternatives && adr.alternatives.length > 0) {
        parts.push(`## Alternatives Considered\n${adr.alternatives.map((item) => `- ${item}`).join('\n')}`);
      }
      if (adr.relatedFiles && adr.relatedFiles.length > 0) {
        parts.push(`## Related Files\n${adr.relatedFiles.map((file) => `- ${file}`).join('\n')}`);
      }
      return parts.join('\n');
    }

    if (record.kind === 'faq') {
      const faq = record.record as FAQRecord;
      const parts = [
        `# FAQ: ${faq.question}`,
        `**ID**: ${faq.id}`,
        `**Category**: ${faq.category}`,
        `**Tags**: ${faq.tags.join(', ')}`,
        '',
        `## Answer\n${faq.answer}`,
      ];
      if (faq.relatedFiles && faq.relatedFiles.length > 0) {
        parts.push(`## Related Files\n${faq.relatedFiles.map((file) => `- ${file}`).join('\n')}`);
      }
      return parts.join('\n');
    }

    const learning = record.record as LearningRecord;
    return [
      `# Learning: ${learning.id}`,
      `**Date**: ${new Date(learning.createdAt).toISOString().split('T')[0]}`,
      '',
      `## Context\n${learning.context}`,
      `## Insight\n${learning.content}`,
    ].join('\n');
  }

  private computeVersion(snapshot: KnowledgeSourceSnapshot): string {
    const digest = createHash('sha1')
      .update(JSON.stringify({
        codeIndex: snapshot.codeIndex,
        adrs: snapshot.adrs,
        faqs: snapshot.faqs,
        learnings: snapshot.learnings,
      }))
      .digest('hex')
      .slice(0, 12);
    return `kbctx_${digest}`;
  }

  private computeSourceUpdatedAt(snapshot: KnowledgeSourceSnapshot): string | undefined {
    const timestamps = [
      snapshot.codeIndex?.lastIndexed ?? 0,
      ...snapshot.adrs.map((adr) => adr.date),
      ...snapshot.faqs.map((faq) => Math.max(faq.updatedAt, faq.createdAt)),
      ...snapshot.learnings.map((learning) => learning.createdAt),
    ].filter((value) => value > 0);

    if (timestamps.length === 0) {
      return undefined;
    }

    return new Date(Math.max(...timestamps)).toISOString();
  }

  private resolveContentBudget(maxTokens: number): number {
    if (!Number.isFinite(maxTokens) || maxTokens <= 0) {
      return 0;
    }
    return Math.max(120, maxTokens - 80);
  }

  private maybeTrimToTokens(content: string, maxTokens?: number): string {
    if (!maxTokens || maxTokens <= 0) {
      return content;
    }
    if (estimateTokenCount(content) <= maxTokens) {
      return content;
    }
    const maxChars = estimateMaxCharsForTokens(maxTokens);
    return `${content.substring(0, maxChars)}...`;
  }

  private captureSnapshot(): KnowledgeSourceSnapshot {
    return {
      codeIndex: this.knowledgeBase.getCodeIndex(),
      adrs: [...this.knowledgeBase.getADRs()],
      faqs: [...this.knowledgeBase.getFAQs()],
      learnings: [...this.knowledgeBase.getLearnings()],
    };
  }

  private normalizeString(value: string | undefined): string | undefined {
    const normalized = typeof value === 'string' ? value.trim() : '';
    return normalized.length > 0 ? normalized : undefined;
  }

  private normalizeOptionalField(value: string | undefined): string | undefined {
    return this.normalizeString(value);
  }
}
