import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { load, dump } from "js-yaml";

export async function mergeUpdateMetadata(inputs, output) {
  if (inputs.length === 0) throw new Error("至少需要一个 Electron 更新元数据文件");
  const documents = await Promise.all(inputs.map(async (path) => load(await readFile(path, "utf8"))));
  const version = documents[0]?.version;
  if (typeof version !== "string" || documents.some((value) => value?.version !== version)) {
    throw new Error("Electron 更新元数据版本不一致");
  }
  const files = [];
  const seen = new Set();
  for (const document of documents) {
    for (const file of document.files ?? []) {
      if (!file?.url || !file?.sha512 || seen.has(file.url)) continue;
      seen.add(file.url);
      files.push(file);
    }
  }
  if (files.length === 0) throw new Error("Electron 更新元数据没有发行文件");
  const merged = {
    ...documents[0],
    files,
    path: files[0].url,
    sha512: files[0].sha512,
  };
  await writeFile(output, dump(merged, { lineWidth: -1, noRefs: true }), "utf8");
  return merged;
}

export async function verifyUpdateMetadata(metadataPath, assetsDirectory) {
  const document = load(await readFile(metadataPath, "utf8"));
  for (const file of document.files ?? []) {
    const bytes = await readFile(join(assetsDirectory, basename(file.url)));
    const actual = createHash("sha512").update(bytes).digest("base64");
    if (actual !== file.sha512) throw new Error(`更新文件哈希不匹配: ${file.url}`);
  }
}

function parseArguments(argv) {
  const inputs = [];
  let output = "";
  let assetsDirectory = "";
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!value) throw new Error(`缺少参数值: ${name}`);
    if (name === "--input") inputs.push(resolve(value));
    else if (name === "--output") output = resolve(value);
    else if (name === "--assets-dir") assetsDirectory = resolve(value);
    else throw new Error(`未知参数: ${name}`);
  }
  if (!output) throw new Error("缺少 --output");
  return { inputs, output, assetsDirectory: assetsDirectory || dirname(output) };
}

if (process.argv[1] && import.meta.url === new URL(`file://${resolve(process.argv[1])}`).href) {
  const args = parseArguments(process.argv.slice(2));
  await mergeUpdateMetadata(args.inputs, args.output);
  await verifyUpdateMetadata(args.output, args.assetsDirectory);
}
