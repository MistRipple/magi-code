import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { HeapSnapshotModel } from "./heap-snapshot";

function fixtureSnapshot(extraStringSelfSize: number): Record<string, unknown> {
  return {
    snapshot: {
      meta: {
        node_fields: ["type", "name", "id", "self_size", "edge_count", "trace_node_id", "detachedness"],
        node_types: [["synthetic", "object", "string"], "string", "number", "number", "number", "number", "number"],
        edge_fields: ["type", "name_or_index", "to_node"],
        edge_types: [["context", "element", "property", "internal", "hidden", "shortcut", "weak"], "string_or_number", "node"],
      },
      node_count: 5,
      edge_count: 4,
    },
    // Root -> Foo -> Bar and two equal string nodes.
    nodes: [
      0, 1, 1, 0, 2, 0, 0,
      1, 2, 3, 10, 2, 0, 0,
      1, 3, 5, 20, 0, 0, 0,
      2, 4, 7, 6, 0, 0, 0,
      2, 4, 9, extraStringSelfSize, 0, 0, 0,
    ],
    edges: [
      2, 5, 7,
      2, 6, 21,
      2, 7, 14,
      2, 8, 28,
    ],
    strings: ["", "root", "Foo", "Bar", "dup", "foo", "dupRoot", "bar", "dupChild"],
  };
}

test("heap snapshot model parses graph analysis primitives deterministically", async () => {
  const directory = await mkdtemp(join(tmpdir(), "magi-heap-fixture-"));
  const basePath = join(directory, "base.heapsnapshot");
  const currentPath = join(directory, "current.heapsnapshot");
  try {
    await writeFile(basePath, JSON.stringify(fixtureSnapshot(8)), "utf8");
    await writeFile(currentPath, JSON.stringify(fixtureSnapshot(10)), "utf8");
    const base = await HeapSnapshotModel.load(basePath);
    const current = await HeapSnapshotModel.load(currentPath);

    assert.equal(base.nodeCount, 5);
    assert.equal(base.edgeCount, 4);
    assert.equal(base.summary().total_self_size, 44);

    const details = base.details(0, 50) as { aggregates: { items: Array<Record<string, unknown>> } };
    const duplicateAggregate = details.aggregates.items.find((item) => item.name === "dup");
    assert.equal(duplicateAggregate?.count, 2);
    assert.equal(duplicateAggregate?.self_size, 14);

    const foo = base.nodes.find((node) => node.name === "Foo");
    const bar = base.nodes.find((node) => node.name === "Bar");
    assert(foo && bar);
    const edgeDetails = base.edges(foo.id) as { edges: { items: Array<Record<string, unknown>> } };
    assert(edgeDetails.edges.items.some((edge) => (edge.to as { node_id?: number }).node_id === bar.id));
    const retainerDetails = base.retainersFor(bar.id) as { retainers: { items: Array<Record<string, unknown>> } };
    assert(retainerDetails.retainers.items.some((edge) => (edge.from as { node_id?: number }).node_id === foo.id));

    const dominatorChain = base.dominators(bar.id) as { chain: Array<{ name: string }> };
    assert.deepEqual(dominatorChain.chain.map((node) => node.name), ["Bar", "Foo", "root"]);
    const paths = base.retainingPaths(bar.id) as { paths: Array<Array<{ name: string }>> };
    assert(paths.paths.some((path) => path.map((node) => node.name).join("/") === "Bar/Foo/root"));

    const duplicateStrings = base.duplicateStrings() as { duplicate_strings: { items: Array<Record<string, unknown>> } };
    assert.equal(duplicateStrings.duplicate_strings.items[0]?.value, "dup");
    const objectDetails = base.objectDetails(bar.id) as { outgoing_edge_count: number; retainer_count: number };
    assert.equal(objectDetails.outgoing_edge_count, 0);
    assert.equal(objectDetails.retainer_count, 1);

    const comparison = base.compare(current) as { changes: Array<Record<string, unknown>> };
    assert(comparison.changes.some((change) => change.name === "dup" && change.self_size_delta === 2));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
