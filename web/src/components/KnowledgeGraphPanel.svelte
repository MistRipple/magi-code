<script lang="ts">
  import CytoscapeRenderer from './diagram/CytoscapeRenderer.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { AgentGraphNodeRef, AgentKnowledgeRelation, AgentKnowledgeRelationDraft, AgentKnowledgeRelationKind, AgentKnowledgeRelationStatus } from '../web/agent-api';

  interface GraphPayload { status?: 'indexing' | 'ready' | 'empty' | 'failed'; reasonCode?: string | null; nodes?: unknown[]; edges?: unknown[]; stats?: { totalNodes?: number; totalEdges?: number; returnedNodes?: number; returnedEdges?: number }; truncated?: boolean; }
  interface GraphNode { id: string; kind: 'workspace' | 'file' | 'symbol' | 'knowledge'; label: string; path?: string; knowledgeId?: string; symbolKind?: string; metadata?: Record<string, string>; }
  interface GraphEdge { id: string; source: string; target: string; kind: string; label: string; origin: string; status: AgentKnowledgeRelationStatus; confidence?: number; evidence: string[]; }
  interface KnowledgeOption { id: string; label: string; kind: string; }
  type GraphView = 'review' | 'explore' | 'history';
  type NodeKindFilter = 'all' | GraphNode['kind'];
  interface Props { payload: GraphPayload | null; knowledgeOptions?: KnowledgeOption[]; relations?: AgentKnowledgeRelation[]; relationMeta?: { totalRelations?: number; truncated?: boolean }; editable?: boolean; graphLoading?: boolean; onRefresh?: () => void; onFocusNode?: (nodeId: string | null) => void; onOpenCode?: (path: string) => void; onOpenKnowledge?: (knowledgeId: string) => void; onCreateRelation?: (relation: AgentKnowledgeRelationDraft) => Promise<void>; onUpdateRelation?: (relation: AgentKnowledgeRelationDraft & { relationId: string }) => Promise<void>; onDeleteRelation?: (relationId: string) => Promise<void>; }

  const relationKinds: AgentKnowledgeRelationKind[] = ['applies_to', 'explains', 'references', 'related_to', 'supersedes', 'contradicts'];
  const MAX_FOCUSED_NODES = 12;
  const MAX_FOCUSED_EDGES = 20;
  const MAX_VISIBLE_NODES = 120;
  const MAX_VISIBLE_EDGES = 240;
  const GRAPH_LABEL_LIMIT = 28;
  let { payload, knowledgeOptions = [], relations = [], relationMeta = {}, editable = false, graphLoading = false, onRefresh, onFocusNode, onOpenCode, onOpenKnowledge, onCreateRelation, onUpdateRelation, onDeleteRelation }: Props = $props();
  let activeView = $state<GraphView>('review');
  let showGlobalGraph = $state(false);
  let searchQuery = $state('');
  let nodeKindFilter = $state<NodeKindFilter>('all');
  let selectedRelationId = $state<string | null>(null);
  let selectedNodeId = $state<string | null>(null);
  let selectedEdgeId = $state<string | null>(null);
  let statusOverrides = $state<Record<string, AgentKnowledgeRelationStatus>>({});
  let observedRelationSignature = $state('');
  let historyStatusFilter = $state<'all' | 'active' | 'dangling' | 'rejected'>('all');
  let actionRelationId = $state<string | null>(null);
  let actionError = $state('');
  let manualExpanded = $state(false);
  let editingRelationId = $state<string | null>(null);
  let sourceNodeId = $state('');
  let targetNodeId = $state('');
  let formKind = $state<AgentKnowledgeRelationKind>('applies_to');
  let formOrigin = $state<AgentKnowledgeRelation['origin']>('explicit_user');
  let formEvidence = $state('');
  let formError = $state('');
  let isSaving = $state(false);
  let deletingRelationId = $state<string | null>(null);

  const nodes = $derived.by(() => {
    const nodeMap = new Map<string, GraphNode>();
    for (const value of Array.isArray(payload?.nodes) ? payload.nodes : []) for (const node of normalizeNode(value)) nodeMap.set(node.id, node);
    for (const option of knowledgeOptions) {
      const id = option.id.trim();
      const label = option.label.trim();
      const nodeId = 'knowledge:' + id;
      if (id && label && !nodeMap.has(nodeId)) nodeMap.set(nodeId, { id: nodeId, kind: 'knowledge', label, knowledgeId: id, metadata: { kind: option.kind } });
    }
    for (const relation of relations) for (const reference of [relation.source, relation.target]) {
      const id = nodeRefId(reference);
      if (!nodeMap.has(id)) nodeMap.set(id, nodeFromReference(reference, Boolean(payload?.truncated)));
    }
    return [...nodeMap.values()].sort((a, b) => a.id.localeCompare(b.id));
  });
  const edges = $derived.by(() => {
    const edgeMap = new Map<string, GraphEdge>();
    for (const value of Array.isArray(payload?.edges) ? payload.edges : []) for (const edge of normalizeEdge(value)) edgeMap.set(edge.id, edge);
    for (const relation of relations) edgeMap.set(relation.relationId, relationToEdge(relation));
    return [...edgeMap.values()].sort((a, b) => a.id.localeCompare(b.id));
  });
  const nodeMap = $derived.by(() => new Map(nodes.map((node) => [node.id, node])));
  const edgeMap = $derived.by(() => new Map(edges.map((edge) => [edge.id, edge])));
  const payloadNodeIds = $derived.by(() => new Set((Array.isArray(payload?.nodes) ? payload.nodes : []).flatMap((value) => normalizeNode(value)).map((node) => node.id)));
  const relationNodeIds = $derived.by(() => new Set(relations.flatMap((relation) => [nodeRefId(relation.source), nodeRefId(relation.target)])));
  const knowledgeNodes = $derived(nodes.filter((node) => node.kind === 'knowledge'));
  const targetNodes = $derived(nodes.filter((node) => node.kind !== 'workspace' && node.id !== sourceNodeId));
  const selectedRelation = $derived(relations.find((relation) => relation.relationId === selectedRelationId) ?? null);
  const selectedNode = $derived(selectedNodeId ? nodeMap.get(selectedNodeId) ?? null : null);
  const selectedEdge = $derived(selectedEdgeId ? edgeMap.get(selectedEdgeId) ?? null : null);
  const query = $derived(searchQuery.trim().toLocaleLowerCase());

  function effectiveStatus(relation: AgentKnowledgeRelation): AgentKnowledgeRelationStatus {
    const override = statusOverrides[relation.relationId];
    if (override) return override;
    const source = nodeMap.get(nodeRefId(relation.source));
    const target = nodeMap.get(nodeRefId(relation.target));
    return source?.metadata?.status === 'dangling' || target?.metadata?.status === 'dangling' ? 'dangling' : relation.status;
  }
  function matchesRelation(relation: AgentKnowledgeRelation): boolean {
    if (!query) return true;
    return [nodeLabel(relation.source), nodeLabel(relation.target), relation.kind, relation.origin, ...relation.evidence].join('\n').toLocaleLowerCase().includes(query);
  }
  function filterRelations(items: AgentKnowledgeRelation[]): AgentKnowledgeRelation[] { return items.filter(matchesRelation).sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0)); }
  const reviewRelations = $derived.by(() => filterRelations(relations.filter((relation) => effectiveStatus(relation) === 'candidate')));
  const historyRelations = $derived.by(() => filterRelations(relations.filter((relation) => effectiveStatus(relation) !== 'candidate' && (historyStatusFilter === 'all' || effectiveStatus(relation) === historyStatusFilter))));
  const candidateCount = $derived(relations.filter((relation) => effectiveStatus(relation) === 'candidate').length);
  const activeCount = $derived(relations.filter((relation) => effectiveStatus(relation) === 'active').length);
  const danglingCount = $derived(relations.filter((relation) => effectiveStatus(relation) === 'dangling').length);
  const rejectedCount = $derived(relations.filter((relation) => effectiveStatus(relation) === 'rejected').length);
  const listedRelations = $derived(activeView === 'review' ? reviewRelations : historyRelations);
  const defaultFocusNodeId = $derived.by(() => {
    const connectedNodeIds = new Set(edges.flatMap((edge) => [edge.source, edge.target]));
    return knowledgeNodes.find((node) => connectedNodeIds.has(node.id))?.id
      ?? nodes.find((node) => connectedNodeIds.has(node.id))?.id
      ?? knowledgeNodes[0]?.id
      ?? nodes[0]?.id
      ?? null;
  });
  const focusAnchorIds = $derived.by(() => {
    const ids = new Set<string>();
    if (selectedRelation) { ids.add(nodeRefId(selectedRelation.source)); ids.add(nodeRefId(selectedRelation.target)); }
    else if (selectedEdge) { ids.add(selectedEdge.source); ids.add(selectedEdge.target); }
    else if (selectedNodeId) ids.add(selectedNodeId);
    else if (reviewRelations[0]) { ids.add(nodeRefId(reviewRelations[0].source)); ids.add(nodeRefId(reviewRelations[0].target)); }
    else if (defaultFocusNodeId) ids.add(defaultFocusNodeId);
    return ids;
  });
  const focusNodeIds = $derived.by(() => {
    const ids = new Set(focusAnchorIds);
    const nearbyEdges = edges
      .filter((edge) => focusAnchorIds.has(edge.source) || focusAnchorIds.has(edge.target))
      .sort((left, right) => {
        const statusRank = (status: AgentKnowledgeRelationStatus): number => status === 'candidate' ? 0 : status === 'active' ? 1 : status === 'dangling' ? 2 : 3;
        return statusRank(left.status) - statusRank(right.status)
          || (right.confidence ?? 0) - (left.confidence ?? 0)
          || left.id.localeCompare(right.id);
      });
    for (const edge of nearbyEdges.slice(0, MAX_FOCUSED_EDGES)) {
      if (ids.size >= MAX_FOCUSED_NODES) break;
      ids.add(edge.source);
      if (ids.size >= MAX_FOCUSED_NODES) break;
      ids.add(edge.target);
    }
    return ids;
  });
  const graphScopeIds = $derived.by(() => showGlobalGraph ? new Set([...payloadNodeIds, ...relationNodeIds]) : focusNodeIds);
  const graphScopeNodes = $derived.by(() => nodes.filter((node) => graphScopeIds.has(node.id) && (nodeKindFilter === 'all' || node.kind === nodeKindFilter) && (!query || node.label.toLocaleLowerCase().includes(query) || node.path?.toLocaleLowerCase().includes(query))));
  const graphNodeLimit = $derived(showGlobalGraph ? MAX_VISIBLE_NODES : MAX_FOCUSED_NODES);
  const graphEdgeLimit = $derived(showGlobalGraph ? MAX_VISIBLE_EDGES : MAX_FOCUSED_EDGES);
  const graphNodeIds = $derived.by(() => new Set(graphScopeNodes.slice(0, graphNodeLimit).map((node) => node.id)));
  const graphScopeEdges = $derived.by(() => edges.filter((edge) => graphNodeIds.has(edge.source) && graphNodeIds.has(edge.target)));
  const clientGraphTruncated = $derived(graphScopeNodes.length > graphNodeLimit || graphScopeEdges.length > graphEdgeLimit);
  const visibleGraph = $derived({
    nodes: nodes.filter((node) => graphNodeIds.has(node.id)).map((node) => ({ id: node.id, label: graphDisplayLabel(node.label), type: node.kind, data: { kind: node.kind, path: node.path ?? '', knowledgeId: node.knowledgeId ?? '', symbolKind: node.symbolKind ?? '' } })),
    edges: graphScopeEdges.slice(0, graphEdgeLimit).map((edge) => ({ id: edge.id, source: edge.source, target: edge.target, label: edge.label, type: edge.kind, data: { kind: edge.kind, status: edge.status, origin: edge.origin } })),
  });
  $effect(() => {
    if (selectedRelationId && !relations.some((relation) => relation.relationId === selectedRelationId)) selectedRelationId = null;
    if (!selectedRelationId && !selectedNodeId && reviewRelations[0]) selectedRelationId = reviewRelations[0].relationId;
  });
  $effect(() => {
    if (editable && !editingRelationId && !sourceNodeId && knowledgeNodes.length) {
      sourceNodeId = knowledgeNodes[0].id;
      targetNodeId = nodes.find((node) => node.kind !== 'workspace' && node.id !== sourceNodeId)?.id ?? '';
    }
  });
  $effect(() => {
    const signature = relations.map((relation) => relation.relationId + ':' + relation.status + ':' + (relation.updatedAt ?? '')).join('|');
    if (signature !== observedRelationSignature) {
      observedRelationSignature = signature;
      statusOverrides = {};
    }
  });

  function normalizeNode(value: unknown): GraphNode[] {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
    const record = value as Record<string, unknown>;
    const id = typeof record.id === 'string' ? record.id : '';
    const kind = typeof record.kind === 'string' ? record.kind : '';
    if (!id || !['workspace', 'file', 'symbol', 'knowledge'].includes(kind)) return [];
    const metadata = record.metadata && typeof record.metadata === 'object' && !Array.isArray(record.metadata) ? Object.fromEntries(Object.entries(record.metadata).flatMap(([key, item]) => typeof item === 'string' ? [[key, item]] : [])) : undefined;
    return [{ id, kind: kind as GraphNode['kind'], label: typeof record.label === 'string' ? record.label : id, path: typeof record.path === 'string' ? record.path : undefined, knowledgeId: typeof record.knowledgeId === 'string' ? record.knowledgeId : undefined, symbolKind: typeof record.symbolKind === 'string' ? record.symbolKind : undefined, metadata }];
  }
  function normalizeEdge(value: unknown): GraphEdge[] {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
    const record = value as Record<string, unknown>;
    if (typeof record.id !== 'string' || typeof record.source !== 'string' || typeof record.target !== 'string') return [];
    return [{ id: record.id, source: record.source, target: record.target, kind: typeof record.kind === 'string' ? record.kind : 'related_to', label: typeof record.label === 'string' ? record.label : '', origin: typeof record.origin === 'string' ? record.origin : 'deterministic_code', status: parseStatus(record.status), confidence: typeof record.confidence === 'number' ? record.confidence : undefined, evidence: Array.isArray(record.evidence) ? record.evidence.filter((item): item is string => typeof item === 'string') : [] }];
  }
  function parseStatus(value: unknown): AgentKnowledgeRelationStatus { return value === 'candidate' || value === 'dangling' || value === 'rejected' || value === 'active' ? value : 'active'; }
  function relationToEdge(relation: AgentKnowledgeRelation): GraphEdge { return { id: relation.relationId, source: nodeRefId(relation.source), target: nodeRefId(relation.target), kind: relation.kind, label: relationKindLabel(relation.kind), origin: relation.origin, status: effectiveStatus(relation), confidence: relation.confidence, evidence: relation.evidence }; }
  function nodeFromReference(reference: AgentGraphNodeRef, graphWasTruncated = false): GraphNode {
    const metadata = graphWasTruncated ? undefined : { status: 'dangling' };
    if (reference.kind === 'knowledge') return { id: nodeRefId(reference), kind: 'knowledge', label: reference.knowledgeId, knowledgeId: reference.knowledgeId, metadata };
    if (reference.kind === 'file') return { id: nodeRefId(reference), kind: 'file', label: reference.path, path: reference.path, metadata };
    return { id: nodeRefId(reference), kind: 'symbol', label: reference.qualifiedName, path: reference.path, symbolKind: reference.symbolKind, metadata };
  }
  function nodeRefId(reference: AgentGraphNodeRef): string { if (reference.kind === 'knowledge') return 'knowledge:' + reference.knowledgeId; if (reference.kind === 'file') return 'file:' + reference.path; return 'symbol:' + reference.path + ':' + reference.qualifiedName + ':' + reference.symbolKind; }
  function nodeRef(node: GraphNode | undefined): AgentGraphNodeRef | null { if (!node) return null; if (node.kind === 'knowledge' && node.knowledgeId) return { kind: 'knowledge', knowledgeId: node.knowledgeId }; if (node.kind === 'file' && node.path) return { kind: 'file', path: node.path }; if (node.kind === 'symbol' && node.path && node.symbolKind) return { kind: 'symbol', path: node.path, qualifiedName: node.label, symbolKind: node.symbolKind }; return null; }
  function nodeLabel(reference: AgentGraphNodeRef): string { return nodeMap.get(nodeRefId(reference))?.label ?? nodeRefId(reference); }
  function relationKindLabel(kind: string): string { return i18n.t('knowledge.graph.relationKind.' + kind); }
  function relationStatusLabel(status: AgentKnowledgeRelationStatus): string { return i18n.t('knowledge.graph.relationStatus.' + status); }
  function relationOriginLabel(origin: string): string { const key = 'knowledge.graph.relationOrigin.' + origin; const value = i18n.t(key); return value === key ? i18n.t('knowledge.graph.relationOrigin.system') : value; }
  function nodeKindIcon(kind: GraphNode['kind']): 'database' | 'file' | 'code' | 'git-branch' { return kind === 'knowledge' ? 'database' : kind === 'file' ? 'file' : kind === 'symbol' ? 'code' : 'git-branch'; }
  function graphDisplayLabel(label: string): string { const compact = label.replace(/\s+/g, ' ').trim(); return compact.length > GRAPH_LABEL_LIMIT ? compact.slice(0, GRAPH_LABEL_LIMIT - 1) + '…' : compact; }
  function isManualRelation(relation: AgentKnowledgeRelation): boolean { return relation.origin === 'explicit_user'; }
  function formatConfidence(value: number | undefined): string { return typeof value === 'number' && Number.isFinite(value) ? String(Math.round(Math.max(0, Math.min(1, value)) * 100)) + '%' : i18n.t('knowledge.graph.confidenceUnavailable'); }
  function formatTimestamp(value: number | undefined): string { return value && Number.isFinite(value) ? new Intl.DateTimeFormat(i18n.locale, { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }).format(new Date(value)) : ''; }
  function chooseRelation(relation: AgentKnowledgeRelation): void { selectedRelationId = relation.relationId; selectedNodeId = null; selectedEdgeId = relation.relationId; showGlobalGraph = false; actionError = ''; onFocusNode?.(nodeRefId(relation.source)); }
  function chooseNode(nodeId: string): void { selectedNodeId = nodeId; selectedRelationId = null; selectedEdgeId = null; activeView = 'explore'; showGlobalGraph = false; onFocusNode?.(nodeId); }
  function handleGraphNodeClick(data: Record<string, unknown>): void { if (typeof data.id === 'string') chooseNode(data.id); }
  function handleGraphEdgeClick(data: Record<string, unknown>): void { if (typeof data.id !== 'string') return; const relation = relations.find((item) => item.relationId === data.id); if (relation) chooseRelation(relation); else { selectedEdgeId = data.id; selectedRelationId = null; selectedNodeId = null; activeView = 'explore'; } }
  function toggleGraphScope(): void { showGlobalGraph = !showGlobalGraph; onFocusNode?.(showGlobalGraph ? null : (selectedNodeId ?? defaultFocusNodeId)); }
  function relationDraft(relation: AgentKnowledgeRelation, status: AgentKnowledgeRelationStatus): AgentKnowledgeRelationDraft & { relationId: string } { return { relationId: relation.relationId, source: relation.source, kind: relation.kind, target: relation.target, origin: relation.origin, confidence: relation.confidence, status, evidence: relation.evidence }; }
  async function updateStatus(relation: AgentKnowledgeRelation, status: AgentKnowledgeRelationStatus): Promise<void> {
    if (!editable || actionRelationId) return;
    actionRelationId = relation.relationId; actionError = '';
    try { await onUpdateRelation?.(relationDraft(relation, status)); statusOverrides = { ...statusOverrides, [relation.relationId]: status }; if (status !== 'candidate') { selectedRelationId = null; selectedEdgeId = null; } }
    catch (error) { console.warn('[KnowledgeGraphPanel] relation review update failed:', error); actionError = i18n.t('knowledge.graph.reviewActionFailed'); }
    finally { actionRelationId = null; }
  }
  function resetForm(): void { editingRelationId = null; sourceNodeId = knowledgeNodes[0]?.id ?? ''; targetNodeId = nodes.find((node) => node.kind !== 'workspace' && node.id !== sourceNodeId)?.id ?? ''; formKind = 'applies_to'; formOrigin = 'explicit_user'; formEvidence = ''; formError = ''; }
  function openManualEntry(relation?: AgentKnowledgeRelation): void { manualExpanded = true; formError = ''; if (!relation) { resetForm(); return; } editingRelationId = relation.relationId; sourceNodeId = nodeRefId(relation.source); targetNodeId = nodeRefId(relation.target); formKind = relation.kind; formOrigin = relation.origin; formEvidence = relation.evidence.join('\n'); }
  function closeManualEntry(): void { manualExpanded = false; resetForm(); }
  async function saveRelation(): Promise<void> {
    if (isSaving) return;
    const source = nodeRef(nodeMap.get(sourceNodeId)); const target = nodeRef(nodeMap.get(targetNodeId));
    if (!source || !target) { formError = i18n.t('knowledge.graph.relationForm.nodeRequired'); return; }
    if (sourceNodeId === targetNodeId) { formError = i18n.t('knowledge.graph.relationForm.nodesMustDiffer'); return; }
    const editingRelation = editingRelationId ? relations.find((relation) => relation.relationId === editingRelationId) : null;
    const status = editingRelation && effectiveStatus(editingRelation) === 'candidate' ? 'active' : editingRelation?.status ?? 'active';
    const draft: AgentKnowledgeRelationDraft = { source, kind: formKind, target, origin: formOrigin, status, evidence: formEvidence.split('\n').map((line) => line.trim()).filter(Boolean) };
    isSaving = true; formError = '';
    try { if (editingRelationId) await onUpdateRelation?.({ ...draft, relationId: editingRelationId }); else await onCreateRelation?.(draft); closeManualEntry(); }
    catch (error) { console.warn('[KnowledgeGraphPanel] save relation failed:', error); formError = i18n.t('knowledge.graph.relationForm.saveFailed'); }
    finally { isSaving = false; }
  }
  async function deleteRelation(relation: AgentKnowledgeRelation): Promise<void> {
    if (deletingRelationId || !isManualRelation(relation)) return;
    deletingRelationId = relation.relationId; formError = '';
    try { await onDeleteRelation?.(relation.relationId); if (editingRelationId === relation.relationId) closeManualEntry(); }
    catch (error) { console.warn('[KnowledgeGraphPanel] delete relation failed:', error); formError = i18n.t('knowledge.graph.relationForm.deleteFailed'); }
    finally { deletingRelationId = null; }
  }
</script>

<div class="knowledge-graph-panel">
  <header class="graph-header">
    <div class="graph-heading">
      <div class="graph-heading-icon"><Icon name="git-branch" size={16} /></div>
      <div>
        <div class="graph-heading-title">{i18n.t('knowledge.graph.discovery.auto')}</div>
        <p>{i18n.t('knowledge.graph.discovery.autoHint')}</p>
      </div>
    </div>
    <div class="graph-header-meta">
      <span class="graph-meta-item"><span class="graph-meta-value">{payload?.stats?.returnedNodes ?? nodes.length}</span>{i18n.t('knowledge.graph.nodes')}</span>
      <span class="graph-meta-item"><span class="graph-meta-value">{relationMeta.totalRelations ?? payload?.stats?.totalEdges ?? edges.length}</span>{i18n.t('knowledge.graph.edges')}</span>
    </div>
  </header>

  {#if payload?.status === 'indexing'}
    <div class="graph-empty"><Icon name="loader" size={24} class="spinning" /><span>{i18n.t('knowledge.graph.indexing')}</span><small>{i18n.t('knowledge.graph.indexingHint')}</small></div>
  {:else if payload?.status === 'failed'}
    <div class="graph-empty"><Icon name="alert-circle" size={28} /><span>{i18n.t('knowledge.graph.failed')}</span><small>{payload.reasonCode ?? i18n.t('knowledge.graph.failedHint')}</small>{#if onRefresh}<button type="button" class="graph-retry" onclick={onRefresh}><Icon name="refresh" size={13} />{i18n.t('knowledge.graph.retry')}</button>{/if}</div>
  {:else if nodes.length === 0}
    <div class="graph-empty"><Icon name="git-branch" size={28} /><span>{i18n.t('knowledge.graph.empty')}</span><small>{i18n.t('knowledge.graph.emptyHint')}</small></div>
  {:else}
    <section class="graph-workspace">
      <div class="graph-statuses" aria-label={i18n.t('knowledge.graph.statusSummary')}>
        <button type="button" class:active={activeView === 'review'} class="status-summary status-summary--candidate" onclick={() => { activeView = 'review'; historyStatusFilter = 'all'; selectedRelationId = null; selectedEdgeId = null; showGlobalGraph = false; }}><Icon name="hourglass" size={14} /><span>{i18n.t('knowledge.graph.relationStatus.candidate')}</span><strong>{candidateCount}</strong></button>
        <button type="button" class:active={activeView === 'history' && historyStatusFilter === 'active'} class="status-summary status-summary--active" onclick={() => { activeView = 'history'; historyStatusFilter = 'active'; selectedRelationId = null; selectedEdgeId = null; showGlobalGraph = false; }}><Icon name="check-circle" size={14} /><span>{i18n.t('knowledge.graph.relationStatus.active')}</span><strong>{activeCount}</strong></button>
        <button type="button" class:active={activeView === 'history' && historyStatusFilter === 'dangling'} class="status-summary status-summary--dangling" onclick={() => { activeView = 'history'; historyStatusFilter = 'dangling'; selectedRelationId = null; selectedEdgeId = null; showGlobalGraph = false; }}><Icon name="alert-triangle" size={14} /><span>{i18n.t('knowledge.graph.relationStatus.dangling')}</span><strong>{danglingCount}</strong></button>
      </div>

      <div class="graph-toolbar">
        <div class="graph-view-tabs" role="tablist" aria-label={i18n.t('knowledge.graph.viewLabel')}>
          <button type="button" role="tab" aria-selected={activeView === 'review'} class:active={activeView === 'review'} onclick={() => { activeView = 'review'; showGlobalGraph = false; }}><Icon name="hourglass" size={13} />{i18n.t('knowledge.graph.view.review')}</button>
          <button type="button" role="tab" aria-selected={activeView === 'explore'} class:active={activeView === 'explore'} onclick={() => { activeView = 'explore'; showGlobalGraph = false; }}><Icon name="git-branch" size={13} />{i18n.t('knowledge.graph.view.focused')}</button>
          <button type="button" role="tab" aria-selected={activeView === 'history'} class:active={activeView === 'history'} onclick={() => { activeView = 'history'; historyStatusFilter = 'all'; showGlobalGraph = false; }}><Icon name="clock" size={13} />{i18n.t('knowledge.graph.view.history')}</button>
        </div>
        <div class="graph-toolbar-actions">
          <label class="graph-search"><Icon name="search" size={13} /><input bind:value={searchQuery} placeholder={i18n.t('knowledge.graph.searchPlaceholder')} aria-label={i18n.t('knowledge.graph.searchPlaceholder')} /></label>
          <select bind:value={nodeKindFilter} aria-label={i18n.t('knowledge.graph.nodeFilter')}>
            <option value="all">{i18n.t('knowledge.graph.nodeFilterAll')}</option>
            <option value="knowledge">{i18n.t('knowledge.graph.nodeKind.knowledge')}</option>
            <option value="file">{i18n.t('knowledge.graph.nodeKind.file')}</option>
            <option value="symbol">{i18n.t('knowledge.graph.nodeKind.symbol')}</option>
          </select>
        </div>
      </div>

      {#if payload?.truncated || clientGraphTruncated || relationMeta.truncated}
        <div class="graph-notice">
          <Icon name="info" size={13} />
          <span>
            {#if relationMeta.truncated}
              {i18n.t('knowledge.graph.relationsTruncated', { shown: relations.length, total: relationMeta.totalRelations ?? relations.length })}
            {:else}
              {i18n.t('knowledge.graph.truncated')}
            {/if}
          </span>
        </div>
      {/if}
      {#if danglingCount > 0 && activeView === 'review'}<button type="button" class="dangling-notice" onclick={() => { activeView = 'history'; showGlobalGraph = false; }}><Icon name="alert-triangle" size={13} /><span>{i18n.t('knowledge.graph.danglingHint', { count: danglingCount })}</span><Icon name="chevron-right" size={13} /></button>{/if}

      <div class="graph-content-grid" class:graph-content-grid--explore={activeView === 'explore'}>
        {#if activeView !== 'explore'}
          <aside class="review-queue">
            <div class="review-queue-header"><div><h4>{activeView === 'review' ? i18n.t('knowledge.graph.reviewTitle') : i18n.t('knowledge.graph.historyTitle')}</h4><p>{activeView === 'review' ? i18n.t('knowledge.graph.reviewHint') : i18n.t('knowledge.graph.historyHint')}</p></div>{#if activeView === 'history' && rejectedCount > 0}<span class="review-queue-count">{rejectedCount} {i18n.t('knowledge.graph.relationStatus.rejected')}</span>{/if}</div>
            {#if listedRelations.length === 0}
              <div class="review-empty"><Icon name={activeView === 'review' ? 'check-circle' : 'git-branch'} size={24} /><strong>{activeView === 'review' ? i18n.t('knowledge.graph.reviewComplete') : i18n.t('knowledge.graph.historyEmpty')}</strong><span>{activeView === 'review' ? i18n.t('knowledge.graph.reviewCompleteHint') : i18n.t('knowledge.graph.historyEmptyHint')}</span></div>
            {:else}
              <div class="review-relation-list">
                {#each listedRelations as relation (relation.relationId)}
                  {@const status = effectiveStatus(relation)}
                  <button type="button" class="review-relation" class:selected={selectedRelationId === relation.relationId} class:dangling={status === 'dangling'} onclick={() => chooseRelation(relation)}>
                    <div class="review-relation-heading"><span class="node-kind-mark"><Icon name={nodeKindIcon(nodeMap.get(nodeRefId(relation.source))?.kind ?? 'knowledge')} size={11} /></span><span>{nodeLabel(relation.source)}</span></div>
                    <div class="review-relation-path"><span>{relationKindLabel(relation.kind)}</span><Icon name="chevron-right" size={12} /><span>{nodeLabel(relation.target)}</span></div>
                    <div class="review-relation-meta"><span class={'relation-status relation-status--' + status}>{relationStatusLabel(status)}</span><span>{relationOriginLabel(relation.origin)}</span>{#if typeof relation.confidence === 'number'}<span>{formatConfidence(relation.confidence)}</span>{/if}</div>
                  </button>
                {/each}
              </div>
            {/if}
          </aside>
        {/if}

        <section class="graph-detail">
          <div class="graph-detail-header"><div><span class="graph-detail-eyebrow">{showGlobalGraph ? i18n.t('knowledge.graph.view.global') : i18n.t('knowledge.graph.view.focused')}</span><h4>{selectedRelation ? i18n.t('knowledge.graph.relationDetail') : selectedNode ? selectedNode.label : i18n.t('knowledge.graph.exploreTitle')}</h4></div><button type="button" class="graph-view-toggle" onclick={toggleGraphScope} title={showGlobalGraph ? i18n.t('knowledge.graph.view.focused') : i18n.t('knowledge.graph.view.global')}><Icon name={showGlobalGraph ? 'target' : 'grid'} size={14} /><span>{showGlobalGraph ? i18n.t('knowledge.graph.view.focused') : i18n.t('knowledge.graph.view.global')}</span></button></div>

          {#if selectedRelation}
            {@const selectedStatus = effectiveStatus(selectedRelation)}
            <article class="relation-detail">
              <div class="relation-detail-path"><button type="button" class="relation-node-chip" onclick={() => chooseNode(nodeRefId(selectedRelation.source))}><Icon name={nodeKindIcon(nodeMap.get(nodeRefId(selectedRelation.source))?.kind ?? 'knowledge')} size={12} /><span>{nodeLabel(selectedRelation.source)}</span></button><span class="relation-kind-flow">{relationKindLabel(selectedRelation.kind)}<Icon name="chevron-right" size={12} /></span><button type="button" class="relation-node-chip" onclick={() => chooseNode(nodeRefId(selectedRelation.target))}><Icon name={nodeKindIcon(nodeMap.get(nodeRefId(selectedRelation.target))?.kind ?? 'file')} size={12} /><span>{nodeLabel(selectedRelation.target)}</span></button></div>
              <div class="relation-detail-meta"><span class={'relation-status relation-status--' + selectedStatus}>{relationStatusLabel(selectedStatus)}</span><span class="relation-origin"><Icon name={selectedRelation.origin === 'explicit_user' ? 'edit' : 'sparkles'} size={11} />{relationOriginLabel(selectedRelation.origin)}</span><span class="relation-confidence"><Icon name="target" size={11} />{i18n.t('knowledge.graph.confidence')} {formatConfidence(selectedRelation.confidence)}</span>{#if formatTimestamp(selectedRelation.updatedAt)}<span>{formatTimestamp(selectedRelation.updatedAt)}</span>{/if}</div>
              <div class="relation-evidence-block"><div class="relation-evidence-title"><Icon name="file-text" size={13} /><span>{i18n.t('knowledge.graph.evidence')}</span></div>{#if selectedRelation.evidence.length > 0}<ul>{#each selectedRelation.evidence as evidence, index (index)}<li>{evidence}</li>{/each}</ul>{:else}<p>{i18n.t('knowledge.graph.evidenceEmpty')}</p>{/if}</div>
              {#if selectedRelation.discoveryEvidence?.length}<div class="relation-evidence-block relation-evidence-block--discovery"><div class="relation-evidence-title"><Icon name="sparkles" size={13} /><span>{i18n.t('knowledge.graph.discoveryEvidence')}</span></div><ul>{#each selectedRelation.discoveryEvidence as evidence, index (index)}<li>{evidence}</li>{/each}</ul></div>{/if}
              {#if editable}<div class="relation-review-actions">
                {#if selectedStatus === 'candidate'}<button type="button" class="relation-confirm-btn" onclick={() => updateStatus(selectedRelation, 'active')} disabled={actionRelationId === selectedRelation.relationId}><Icon name="check" size={13} />{actionRelationId === selectedRelation.relationId ? i18n.t('knowledge.graph.reviewSaving') : i18n.t('knowledge.graph.confirm')}</button><button type="button" class="relation-secondary-btn" onclick={() => openManualEntry(selectedRelation)} disabled={actionRelationId === selectedRelation.relationId}><Icon name="edit" size={13} />{i18n.t('knowledge.graph.correct')}</button><button type="button" class="relation-ignore-btn" onclick={() => updateStatus(selectedRelation, 'rejected')} disabled={actionRelationId === selectedRelation.relationId}><Icon name="thumbs-down" size={13} />{i18n.t('knowledge.graph.ignore')}</button>
                {:else if selectedStatus === 'rejected'}<button type="button" class="relation-secondary-btn" onclick={() => updateStatus(selectedRelation, 'candidate')} disabled={actionRelationId === selectedRelation.relationId}><Icon name="undo" size={13} />{i18n.t('knowledge.graph.restoreReview')}</button>
                {:else if selectedStatus === 'dangling'}<button type="button" class="relation-secondary-btn" onclick={() => openManualEntry(selectedRelation)}><Icon name="edit" size={13} />{i18n.t('knowledge.graph.repairRelation')}</button><button type="button" class="relation-ignore-btn" onclick={() => updateStatus(selectedRelation, 'rejected')} disabled={actionRelationId === selectedRelation.relationId}><Icon name="thumbs-down" size={13} />{i18n.t('knowledge.graph.ignore')}</button>
                {:else}<span class="relation-active-note"><Icon name="check-circle" size={13} />{i18n.t('knowledge.graph.confirmedHint')}</span>{/if}
                {#if selectedStatus !== 'candidate' && selectedStatus !== 'dangling'}<button type="button" class="relation-icon-button" onclick={() => openManualEntry(selectedRelation)} title={i18n.t('knowledge.actions.edit')}><Icon name="edit" size={13} /></button>{/if}
                {#if isManualRelation(selectedRelation)}<button type="button" class="relation-icon-button relation-icon-button--danger" onclick={() => deleteRelation(selectedRelation)} disabled={deletingRelationId === selectedRelation.relationId} title={i18n.t('knowledge.graph.relationForm.delete')}><Icon name="trash" size={13} /></button>{/if}
              </div>{/if}
              {#if actionError}<div class="relation-action-error"><Icon name="warning" size={13} />{actionError}</div>{/if}
            </article>
          {:else if selectedNode}
            <article class="node-detail"><span class="node-detail-icon"><Icon name={nodeKindIcon(selectedNode.kind)} size={15} /></span><div class="node-detail-body"><strong>{selectedNode.label}</strong><p>{i18n.t('knowledge.graph.nodeKind.' + selectedNode.kind)}{#if selectedNode.path} · {selectedNode.path}{/if}</p>{#if selectedNode.path || selectedNode.knowledgeId}<div class="node-detail-actions">{#if selectedNode.path}<button type="button" class="relation-secondary-btn" onclick={() => onOpenCode?.(selectedNode.path ?? '')}><Icon name="code" size={12} />{i18n.t('knowledge.graph.openCode')}</button>{/if}{#if selectedNode.knowledgeId}<button type="button" class="relation-secondary-btn" onclick={() => onOpenKnowledge?.(selectedNode.knowledgeId ?? '')}><Icon name="database" size={12} />{i18n.t('knowledge.graph.openKnowledge')}</button>{/if}</div>{/if}</div>{#if selectedNode.metadata?.status === 'dangling'}<span class="relation-status relation-status--dangling">{i18n.t('knowledge.graph.relationStatus.dangling')}</span>{/if}</article>
          {:else if selectedEdge}
            <article class="node-detail"><span class="node-detail-icon"><Icon name="git-branch" size={15} /></span><div><strong>{selectedEdge.label || relationKindLabel(selectedEdge.kind)}</strong><p>{relationOriginLabel(selectedEdge.origin)} · {formatConfidence(selectedEdge.confidence)}</p></div></article>
          {/if}

          <div class="graph-canvas" class:graph-canvas--global={showGlobalGraph}><CytoscapeRenderer graph={visibleGraph} layout={showGlobalGraph ? 'auto' : 'breadthfirst'} rootNodeId={showGlobalGraph ? null : (selectedRelation ? nodeRefId(selectedRelation.source) : selectedNodeId ?? defaultFocusNodeId)} onNodeClick={handleGraphNodeClick} onEdgeClick={handleGraphEdgeClick} />{#if graphLoading}<div class="graph-loading"><Icon name="loader" size={14} class="spinning" />{i18n.t('knowledge.graph.loadingFocused')}</div>{/if}</div>
          <div class="graph-legend"><span><i class="legend-dot legend-dot--knowledge"></i>{i18n.t('knowledge.graph.nodeKind.knowledge')}</span><span><i class="legend-dot legend-dot--file"></i>{i18n.t('knowledge.graph.nodeKind.file')}</span><span><i class="legend-dot legend-dot--symbol"></i>{i18n.t('knowledge.graph.nodeKind.symbol')}</span><span><i class="legend-line legend-line--candidate"></i>{i18n.t('knowledge.graph.relationStatus.candidate')}</span></div>
        </section>
      </div>

      {#if editable}
        <section class="manual-relation-section"><button type="button" class="manual-relation-toggle" aria-expanded={manualExpanded} onclick={() => manualExpanded ? closeManualEntry() : openManualEntry()}><Icon name={manualExpanded ? 'chevron-up' : 'plus'} size={13} /><span>{manualExpanded ? i18n.t('knowledge.graph.manualAdd.collapse') : i18n.t('knowledge.graph.manualAdd.expand')}</span></button>
          {#if manualExpanded}<div class="relation-form"><div class="relation-form-intro"><Icon name="info" size={13} /><span>{i18n.t('knowledge.graph.manualAdd.hint')}</span></div><div class="relation-form-grid"><label><span>{i18n.t('knowledge.graph.relationForm.source')}</span><select bind:value={sourceNodeId}><option value="">{i18n.t('knowledge.graph.relationForm.chooseSource')}</option>{#each knowledgeNodes as node (node.id)}<option value={node.id}>{i18n.t('knowledge.graph.nodeKind.' + node.kind)} · {node.label}</option>{/each}</select></label><label><span>{i18n.t('knowledge.graph.relationForm.kind')}</span><select bind:value={formKind}>{#each relationKinds as kind}<option value={kind}>{relationKindLabel(kind)}</option>{/each}</select></label><label><span>{i18n.t('knowledge.graph.relationForm.target')}</span><select bind:value={targetNodeId}><option value="">{i18n.t('knowledge.graph.relationForm.chooseTarget')}</option>{#each targetNodes as node (node.id)}<option value={node.id}>{i18n.t('knowledge.graph.nodeKind.' + node.kind)} · {node.label}</option>{/each}</select></label></div><label class="relation-evidence-input"><span>{i18n.t('knowledge.graph.relationForm.evidence')}</span><textarea bind:value={formEvidence} rows="2" placeholder={i18n.t('knowledge.graph.relationForm.evidencePlaceholder')}></textarea></label>{#if formError}<div class="relation-form-error"><Icon name="warning" size={13} />{formError}</div>{/if}<div class="relation-form-actions"><button type="button" class="relation-secondary-btn" onclick={closeManualEntry} disabled={isSaving}>{i18n.t('knowledge.actions.cancel')}</button><button type="button" class="relation-confirm-btn" onclick={saveRelation} disabled={isSaving || knowledgeNodes.length === 0}>{isSaving ? i18n.t('knowledge.graph.relationForm.saving') : editingRelationId ? i18n.t('knowledge.graph.relationForm.update') : i18n.t('knowledge.graph.relationForm.create')}</button></div></div>{/if}
        </section>
      {/if}
    </section>
  {/if}
</div>

<style>
  .knowledge-graph-panel { display:flex; flex-direction:column; gap:var(--space-3,12px); min-width:0; }
  .graph-header { display:flex; align-items:flex-start; justify-content:space-between; gap:16px; padding:12px 0; }
  .graph-heading { display:flex; min-width:0; gap:10px; }.graph-heading-icon,.node-detail-icon { display:inline-flex; align-items:center; justify-content:center; flex:0 0 auto; width:28px; height:28px; border:1px solid color-mix(in srgb,var(--primary) 34%,var(--border)); border-radius:var(--radius-sm); color:var(--primary); background:color-mix(in srgb,var(--primary) 9%,transparent); }.graph-heading-title { color:var(--foreground); font-size:var(--text-sm,13px); font-weight:var(--font-semibold,600); }.graph-heading p { margin:3px 0 0; color:var(--foreground-muted); font-size:var(--text-xs,12px); line-height:1.45; }.graph-header-meta { display:flex; flex:0 0 auto; gap:12px; padding-top:4px; }.graph-meta-item { display:flex; align-items:baseline; gap:4px; color:var(--foreground-muted); font-size:10px; white-space:nowrap; }.graph-meta-value { color:var(--foreground); font-size:13px; font-weight:var(--font-semibold,600); }
  .graph-workspace { display:flex; flex-direction:column; gap:12px; min-width:0; }.graph-statuses { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:8px; }.status-summary { display:flex; align-items:center; gap:7px; min-width:0; height:34px; padding:0 10px; border:1px solid var(--border); border-radius:var(--radius-sm); background:var(--surface-1); color:var(--foreground-muted); font-size:var(--text-xs,12px); cursor:pointer; text-align:left; }.status-summary span { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }.status-summary strong { margin-left:auto; color:var(--foreground); font-size:13px; }.status-summary:hover,.status-summary.active { border-color:color-mix(in srgb,var(--primary) 48%,var(--border)); background:var(--surface-2); color:var(--foreground); }.status-summary--candidate :global(svg) { color:var(--warning,#c47700); }.status-summary--active :global(svg) { color:var(--success,#16825d); }.status-summary--dangling :global(svg) { color:var(--danger,#b42318); }
  .graph-toolbar { display:flex; align-items:center; justify-content:space-between; gap:12px; min-width:0; }.graph-view-tabs { display:inline-flex; flex:0 0 auto; gap:2px; padding:3px; border:1px solid var(--border); border-radius:var(--radius-sm); background:var(--surface-1); }.graph-view-tabs button { display:inline-flex; align-items:center; justify-content:center; gap:5px; min-height:26px; padding:0 8px; border:0; border-radius:calc(var(--radius-sm) - 2px); background:transparent; color:var(--foreground-muted); font-size:var(--text-xs,12px); cursor:pointer; white-space:nowrap; }.graph-view-tabs button:hover { color:var(--foreground); }.graph-view-tabs button.active { background:var(--surface-2); color:var(--foreground); box-shadow:0 1px 2px rgba(0,0,0,.08); }.graph-toolbar-actions { display:flex; align-items:center; justify-content:flex-end; gap:8px; min-width:0; }.graph-search { display:flex; align-items:center; gap:6px; width:min(220px,28vw); height:30px; padding:0 8px; border:1px solid var(--border); border-radius:var(--radius-sm); background:var(--surface-1); color:var(--foreground-muted); }.graph-search:focus-within { border-color:var(--primary); }.graph-search input { min-width:0; width:100%; border:0; outline:0; background:transparent; color:var(--foreground); font:inherit; font-size:var(--text-xs,12px); }.graph-toolbar select,.relation-form select,.relation-form textarea { box-sizing:border-box; border:1px solid var(--border); border-radius:var(--radius-sm); background:var(--surface-1); color:var(--foreground); font:inherit; font-size:var(--text-xs,12px); }.graph-toolbar select { height:30px; max-width:110px; padding:0 6px; }
  .graph-notice,.dangling-notice { display:flex; align-items:center; gap:6px; min-height:30px; padding:0 9px; border:1px solid color-mix(in srgb,var(--warning,#c47700) 46%,var(--border)); border-radius:var(--radius-sm); color:var(--warning,#c47700); background:color-mix(in srgb,var(--warning,#c47700) 8%,transparent); font-size:var(--text-xs,12px); }.dangling-notice { width:100%; justify-content:flex-start; cursor:pointer; text-align:left; }.dangling-notice > :last-child { margin-left:auto; }
  .graph-content-grid { display:grid; grid-template-columns:minmax(230px,.8fr) minmax(0,1.6fr); min-height:420px; border:1px solid var(--border); border-radius:var(--radius-md); overflow:hidden; background:var(--surface-1); }.graph-content-grid--explore { grid-template-columns:minmax(0,1fr); }.review-queue { display:flex; flex-direction:column; min-width:0; border-right:1px solid var(--border); background:var(--surface-1); }.review-queue-header { display:flex; align-items:flex-start; justify-content:space-between; gap:8px; padding:12px; border-bottom:1px solid var(--border); }.review-queue-header h4,.graph-detail-header h4 { margin:0; color:var(--foreground); font-size:var(--text-sm,13px); }.review-queue-header p { margin:3px 0 0; color:var(--foreground-muted); font-size:10px; line-height:1.4; }.review-queue-count { flex:0 0 auto; padding:2px 5px; border-radius:var(--radius-sm); background:var(--surface-2); color:var(--foreground-muted); font-size:10px; white-space:nowrap; }.review-relation-list { display:flex; flex-direction:column; gap:2px; max-height:612px; overflow:auto; padding:5px; }.review-relation { display:flex; flex-direction:column; align-items:stretch; gap:5px; min-width:0; padding:9px; border:1px solid transparent; border-radius:var(--radius-sm); background:transparent; color:var(--foreground); text-align:left; cursor:pointer; }.review-relation:hover { background:var(--surface-2); }.review-relation.selected { border-color:color-mix(in srgb,var(--primary) 54%,var(--border)); background:color-mix(in srgb,var(--primary) 8%,var(--surface-1)); }.review-relation.dangling { border-color:color-mix(in srgb,var(--danger,#b42318) 42%,var(--border)); }.review-relation-heading,.review-relation-path { display:flex; align-items:center; gap:6px; min-width:0; font-size:var(--text-xs,12px); }.review-relation-heading > span:last-child,.review-relation-path span { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }.node-kind-mark { display:inline-flex; align-items:center; justify-content:center; width:17px; height:17px; border-radius:4px; background:var(--surface-2); color:var(--foreground-muted); }.review-relation-path { padding-left:23px; color:var(--foreground-muted); font-size:10px; }.review-relation-meta { display:flex; align-items:center; flex-wrap:wrap; gap:5px; padding-left:23px; color:var(--foreground-muted); font-size:10px; }.review-empty { display:flex; flex:1; flex-direction:column; align-items:center; justify-content:center; gap:7px; min-height:210px; padding:16px; color:var(--foreground-muted); font-size:var(--text-xs,12px); text-align:center; }.review-empty :global(svg) { color:var(--success,#16825d); }.review-empty strong { color:var(--foreground); font-size:var(--text-sm,13px); }
  .graph-detail { display:flex; flex-direction:column; min-width:0; background:var(--surface-0,var(--surface-1)); }.graph-detail-header { display:flex; align-items:flex-start; justify-content:space-between; gap:12px; padding:12px; border-bottom:1px solid var(--border); }.graph-detail-eyebrow { display:block; margin-bottom:4px; color:var(--foreground-muted); font-size:10px; }.graph-view-toggle { display:inline-flex; align-items:center; gap:5px; height:27px; padding:0 8px; border:1px solid var(--border); border-radius:var(--radius-sm); background:transparent; color:var(--foreground-muted); font-size:10px; cursor:pointer; }.graph-view-toggle:hover { background:var(--surface-2); color:var(--foreground); }.relation-detail,.node-detail { margin:12px; padding:12px; border:1px solid var(--border); border-radius:var(--radius-sm); background:var(--surface-1); }.relation-detail { display:flex; flex-direction:column; gap:12px; }.relation-detail-path { display:flex; align-items:center; gap:7px; min-width:0; }.relation-node-chip { display:inline-flex; align-items:center; min-width:0; max-width:42%; gap:5px; padding:0; border:0; background:transparent; color:var(--foreground); font-size:var(--text-xs,12px); cursor:pointer; }.relation-node-chip:hover { color:var(--primary); }.relation-node-chip span { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }.relation-kind-flow { display:inline-flex; align-items:center; gap:3px; flex:0 1 auto; min-width:0; color:var(--foreground-muted); font-size:10px; }.relation-detail-meta { display:flex; align-items:center; flex-wrap:wrap; gap:6px; color:var(--foreground-muted); font-size:10px; }.relation-status,.relation-origin,.relation-confidence { display:inline-flex; align-items:center; gap:4px; min-height:19px; padding:0 6px; border-radius:var(--radius-sm); background:var(--surface-2); color:var(--foreground-muted); font-size:10px; }.relation-status--candidate { color:var(--warning,#c47700); background:color-mix(in srgb,var(--warning,#c47700) 11%,transparent); }.relation-status--active { color:var(--success,#16825d); background:color-mix(in srgb,var(--success,#16825d) 10%,transparent); }.relation-status--dangling { color:var(--danger,#b42318); background:color-mix(in srgb,var(--danger,#b42318) 10%,transparent); }.relation-status--rejected { opacity:.72; }.relation-evidence-block { padding-top:8px; border-top:1px solid var(--border); }.relation-evidence-title { display:flex; align-items:center; gap:5px; color:var(--foreground); font-size:var(--text-xs,12px); font-weight:var(--font-medium,500); }.relation-evidence-block ul { display:flex; flex-direction:column; gap:4px; margin:7px 0 0; padding:0 0 0 17px; color:var(--foreground-muted); font-size:var(--text-xs,12px); line-height:1.45; }.relation-evidence-block p { margin:7px 0 0; color:var(--foreground-muted); font-size:var(--text-xs,12px); }.relation-review-actions { display:flex; align-items:center; flex-wrap:wrap; gap:7px; }.relation-confirm-btn,.relation-ignore-btn,.relation-secondary-btn { display:inline-flex; align-items:center; justify-content:center; gap:6px; min-height:28px; padding:0 9px; border-radius:var(--radius-sm); font-size:var(--text-xs,12px); cursor:pointer; }.relation-confirm-btn { border:1px solid var(--primary); background:var(--primary); color:var(--primary-foreground,#fff); }.relation-ignore-btn,.relation-secondary-btn { border:1px solid var(--border); background:transparent; color:var(--foreground-muted); }.relation-confirm-btn:disabled,.relation-ignore-btn:disabled,.relation-secondary-btn:disabled { opacity:.55; cursor:default; }.relation-ignore-btn:hover,.relation-secondary-btn:hover { background:var(--surface-2); color:var(--foreground); }.relation-active-note { display:inline-flex; align-items:center; gap:5px; color:var(--success,#16825d); font-size:var(--text-xs,12px); }.relation-icon-button { display:inline-flex; align-items:center; justify-content:center; width:28px; height:28px; margin-left:auto; border:1px solid var(--border); border-radius:var(--radius-sm); background:transparent; color:var(--foreground-muted); cursor:pointer; }.relation-icon-button + .relation-icon-button { margin-left:0; }.relation-icon-button:hover { background:var(--surface-2); color:var(--foreground); }.relation-icon-button--danger:hover { color:var(--danger,#b42318); }.relation-action-error,.relation-form-error { display:flex; align-items:center; gap:5px; color:var(--danger,#b42318); font-size:var(--text-xs,12px); }.node-detail { display:flex; align-items:flex-start; gap:9px; }.node-detail-icon { width:27px; height:27px; }.node-detail-body { min-width:0; flex:1; }.node-detail strong { display:block; color:var(--foreground); font-size:var(--text-xs,12px); overflow-wrap:anywhere; }.node-detail p { margin:3px 0 0; color:var(--foreground-muted); font-size:10px; overflow-wrap:anywhere; }.node-detail > .relation-status { margin-left:auto; }.node-detail-actions { display:flex; flex-wrap:wrap; gap:6px; margin-top:8px; }.node-detail-actions .relation-secondary-btn { min-height:26px; padding:0 8px; }
  .graph-canvas { position:relative; min-height:420px; margin-top:auto; overflow:hidden; border-top:1px solid var(--border); }.graph-canvas--global { min-height:520px; }.graph-canvas :global(.graph-content) { min-height:420px; }.graph-canvas :global(.cy-host) { height:420px; }.graph-canvas--global :global(.cy-host) { height:520px; }.graph-loading { position:absolute; top:10px; right:10px; display:inline-flex; align-items:center; gap:5px; min-height:26px; padding:0 8px; border:1px solid var(--border); border-radius:var(--radius-sm); background:color-mix(in srgb,var(--surface-1) 90%,transparent); color:var(--foreground-muted); font-size:10px; pointer-events:none; }.graph-legend { display:flex; align-items:center; flex-wrap:wrap; gap:8px 13px; padding:8px 12px; border-top:1px solid var(--border); color:var(--foreground-muted); font-size:10px; }.graph-legend span { display:inline-flex; align-items:center; gap:5px; }.legend-dot { width:7px; height:7px; border-radius:50%; }.legend-dot--knowledge { background:#2563eb; }.legend-dot--file { background:#16825d; }.legend-dot--symbol { background:#c47700; }.legend-line { display:inline-block; width:13px; border-top:2px solid var(--foreground-muted); }.legend-line--candidate { border-top-style:dashed; border-color:var(--warning,#c47700); }
  .manual-relation-section { border:1px solid var(--border); border-radius:var(--radius-md); background:var(--surface-1); }.manual-relation-toggle { display:flex; align-items:center; gap:6px; width:100%; min-height:36px; padding:0 11px; border:0; background:transparent; color:var(--foreground-muted); font-size:var(--text-xs,12px); cursor:pointer; text-align:left; }.manual-relation-toggle:hover { color:var(--foreground); background:var(--surface-2); }.relation-form { display:flex; flex-direction:column; gap:12px; padding:0 12px 12px; }.relation-form-intro { display:flex; align-items:flex-start; gap:6px; color:var(--foreground-muted); font-size:10px; line-height:1.4; }.relation-form-grid { display:grid; grid-template-columns:minmax(0,1fr) minmax(120px,.65fr) minmax(0,1fr); gap:8px; }.relation-form label { display:flex; flex-direction:column; gap:5px; min-width:0; }.relation-form label > span { color:var(--foreground-muted); font-size:10px; }.relation-form select { height:30px; padding:0 7px; }.relation-form textarea { resize:vertical; min-height:48px; padding:7px; }.relation-form-actions { display:flex; justify-content:flex-end; gap:8px; }
  .graph-empty { display:flex; flex-direction:column; align-items:center; justify-content:center; gap:8px; min-height:320px; padding:20px; color:var(--foreground-muted); text-align:center; }.graph-empty small { max-width:360px; font-size:var(--text-xs,12px); }.graph-retry { display:inline-flex; align-items:center; gap:5px; min-height:28px; padding:0 9px; border:1px solid var(--border); border-radius:var(--radius-sm); background:var(--surface-1); color:var(--foreground); font-size:var(--text-xs,12px); cursor:pointer; }.graph-retry:hover { background:var(--surface-2); }.spinning { animation:graph-spin 1s linear infinite; } @keyframes graph-spin { to { transform:rotate(360deg); } }
  @media (max-width:760px) { .graph-header { flex-direction:column; gap:8px; }.graph-header-meta { width:100%; padding-top:0; }.graph-toolbar { align-items:stretch; flex-direction:column; }.graph-view-tabs { width:100%; }.graph-view-tabs button { flex:1; }.graph-toolbar-actions { justify-content:stretch; }.graph-search { width:auto; flex:1; }.graph-content-grid { grid-template-columns:minmax(0,1fr); }.review-queue { border-right:0; border-bottom:1px solid var(--border); }.review-relation-list { max-height:230px; }.graph-canvas,.graph-canvas :global(.graph-content),.graph-canvas :global(.cy-host) { min-height:360px; height:360px; }.graph-canvas--global,.graph-canvas--global :global(.cy-host) { min-height:420px; height:420px; }.relation-form-grid { grid-template-columns:1fr; }.relation-detail-path { align-items:flex-start; flex-wrap:wrap; }.relation-node-chip { max-width:100%; }.relation-kind-flow { width:100%; }.status-summary { padding:0 7px; }.status-summary span { font-size:10px; }.graph-view-toggle span { display:none; } }
  @media (max-width:420px) { .graph-statuses { gap:5px; }.status-summary { gap:4px; }.status-summary strong { font-size:var(--text-xs,12px); }.graph-view-tabs button { padding:0 5px; font-size:10px; }.graph-toolbar-actions { gap:5px; }.graph-toolbar select { max-width:96px; }.relation-review-actions { align-items:stretch; }.relation-confirm-btn,.relation-ignore-btn,.relation-secondary-btn { flex:1; }.relation-icon-button { margin-left:0; }.graph-legend { gap:6px 9px; } }
</style>
