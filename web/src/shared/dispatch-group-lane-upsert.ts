export interface DispatchLaneUpsertUpdate<TLane extends { laneId: string; laneVersion?: number }> {
  dispatchWaveId: string;
  laneId: string;
  laneVersion: number;
  patch: Partial<TLane>;
}

export interface DispatchGroupLike<TLane extends { laneId: string; laneVersion?: number }> {
  type?: string;
  dispatchWaveId?: string;
  lanes?: TLane[];
}

export function upsertDispatchGroupLane<
  TBlock extends DispatchGroupLike<TLane>,
  TLane extends { laneId: string; laneVersion?: number },
>(
  blocks: TBlock[] | undefined,
  update: DispatchLaneUpsertUpdate<TLane>,
): TBlock[] {
  const source = Array.isArray(blocks) ? blocks : [];

  return source.map((block) => {
    if (block.type !== 'dispatch_group' || block.dispatchWaveId !== update.dispatchWaveId) {
      return block;
    }

    const lanes = Array.isArray(block.lanes) ? block.lanes : [];
    const existingLaneIndex = lanes.findIndex((lane) => lane.laneId === update.laneId);

    if (existingLaneIndex < 0) {
      const nextLane = {
        laneId: update.laneId,
        laneVersion: update.laneVersion,
        ...update.patch,
      } as TLane;

      return {
        ...block,
        lanes: [...lanes, nextLane],
      };
    }

    const existingLane = lanes[existingLaneIndex];
    if ((existingLane.laneVersion ?? 0) >= update.laneVersion) {
      return block;
    }

    const nextLanes = [...lanes];
    nextLanes[existingLaneIndex] = {
      ...existingLane,
      ...update.patch,
      laneVersion: update.laneVersion,
    };

    return {
      ...block,
      lanes: nextLanes,
    };
  });
}
