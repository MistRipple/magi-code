import { getTransport } from '../../shared/transport';
import type { AgentBindingOverride } from '../agent-binding-context';
import type { AgentKnowledgeGraphQuery, AgentKnowledgeItemPatch, AgentKnowledgeItemPayload, AgentKnowledgeMutationPayload, AgentKnowledgeRelation, AgentKnowledgeRelationDraft, AgentKnowledgeRelationsResponse } from './types';
import { agentUrl, buildBoundQueryWithOverride, parseAgentJson, postWorkspaceBoundJson } from './core';

export async function clearAgentProjectKnowledge(
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/clear',
    {},
    'clear project knowledge',
    bindingOverride,
  );
}

export async function getAgentProjectKnowledge(
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const query = buildBoundQueryWithOverride({}, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load project knowledge');
}

/** 按节点重新加载知识图谱局部视图，避免前端从首屏全量图中做假聚焦。 */
export async function getAgentKnowledgeGraph(
  graphQuery: AgentKnowledgeGraphQuery = {},
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const params: Record<string, string> = {};
  if (graphQuery.focus?.trim()) params.focus = graphQuery.focus.trim();
  if (typeof graphQuery.depth === 'number' && Number.isFinite(graphQuery.depth)) params.depth = String(Math.trunc(graphQuery.depth));
  if (graphQuery.direction) params.direction = graphQuery.direction;
  if (graphQuery.nodeKinds?.length) params.nodeKinds = graphQuery.nodeKinds.join(',');
  if (graphQuery.edgeKinds?.length) params.edgeKinds = graphQuery.edgeKinds.join(',');
  if (typeof graphQuery.maxNodes === 'number' && Number.isFinite(graphQuery.maxNodes)) params.maxNodes = String(Math.trunc(graphQuery.maxNodes));
  if (typeof graphQuery.maxEdges === 'number' && Number.isFinite(graphQuery.maxEdges)) params.maxEdges = String(Math.trunc(graphQuery.maxEdges));
  const query = buildBoundQueryWithOverride(params, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge/graph', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load knowledge graph');
}

export async function reindexAgentProjectKnowledge(
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>(
    '/api/knowledge/reindex',
    {},
    'reindex project knowledge',
    bindingOverride,
  );
}

export async function addAgentKnowledgeItem(
  payload: AgentKnowledgeItemPayload,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/items',
    { ...payload },
    'add knowledge item',
    bindingOverride,
  );
}

export async function updateAgentKnowledgeItem(
  knowledgeId: string,
  patch: AgentKnowledgeItemPatch,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/items/update',
    { knowledgeId, ...patch },
    'update knowledge item',
    bindingOverride,
  );
}

export async function deleteAgentKnowledgeItem(
  knowledgeId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/items/delete',
    { knowledgeId },
    'delete knowledge item',
    bindingOverride,
  );
}

export async function listAgentKnowledgeRelations(
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeRelationsResponse> {
  const query = buildBoundQueryWithOverride({}, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge/relations', query));
  const payload = await parseAgentJson<Partial<AgentKnowledgeRelationsResponse>>(response, 'list knowledge relations');
  const relations = Array.isArray(payload.relations)
    ? payload.relations.map((relation) => ({
        ...relation,
        evidence: Array.isArray(relation.evidence) ? relation.evidence : [],
      }))
    : [];
  const totalRelations = typeof payload.totalRelations === 'number' && Number.isFinite(payload.totalRelations)
    ? Math.max(relations.length, Math.trunc(payload.totalRelations))
    : relations.length;
  return {
    relations,
    totalRelations,
    truncated: payload.truncated === true || totalRelations > relations.length,
  };
}

export async function addAgentKnowledgeRelation(
  relation: AgentKnowledgeRelationDraft,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeRelation> {
  const payload = await postWorkspaceBoundJson<{ relation?: AgentKnowledgeRelation }>(
    '/api/knowledge/relations',
    relation as unknown as Record<string, unknown>,
    'add knowledge relation',
    bindingOverride,
  );
  if (!payload.relation) {
    throw new Error('知识关系创建响应缺少 relation');
  }
  return payload.relation;
}

export async function updateAgentKnowledgeRelation(
  relation: AgentKnowledgeRelationDraft & { relationId: string },
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeRelation> {
  const payload = await postWorkspaceBoundJson<{ relation?: AgentKnowledgeRelation }>(
    '/api/knowledge/relations/update',
    relation as unknown as Record<string, unknown>,
    'update knowledge relation',
    bindingOverride,
  );
  if (!payload.relation) {
    throw new Error('知识关系更新响应缺少 relation');
  }
  return payload.relation;
}

export async function deleteAgentKnowledgeRelation(
  relationId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<void> {
  await postWorkspaceBoundJson<Record<string, unknown>>(
    '/api/knowledge/relations/delete',
    { relationId },
    'delete knowledge relation',
    bindingOverride,
  );
}
