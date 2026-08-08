const VERSION_PATTERN = /^(?<major>0|[1-9]\d*)\.(?<minor>0|[1-9]\d*)\.(?<patch>0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const MAJOR_FACTOR = 1_000_000_000_000n;
const MINOR_FACTOR = 1_000_000n;
const COMPONENT_LIMIT = 1_000_000n;
const MAX_SAFE_SEQUENCE = BigInt(Number.MAX_SAFE_INTEGER);

function releaseSequenceFloor(version) {
  const match = VERSION_PATTERN.exec(version.trim());
  if (!match?.groups) {
    throw new Error(`产品版本不是有效的 SemVer：${version}`);
  }

  const major = BigInt(match.groups.major);
  const minor = BigInt(match.groups.minor);
  const patch = BigInt(match.groups.patch);
  if (minor >= COMPONENT_LIMIT || patch >= COMPONENT_LIMIT) {
    throw new Error(`产品版本的次版本或修订号不能大于 ${COMPONENT_LIMIT - 1n}：${version}`);
  }

  const sequence = major * MAJOR_FACTOR + minor * MINOR_FACTOR + patch;
  if (sequence > MAX_SAFE_SEQUENCE) {
    throw new Error(`产品版本生成的清单序列超过安全整数范围：${version}`);
  }
  return sequence;
}

const version = process.argv[2];
if (!version) {
  throw new Error('用法：node scripts/browser-runtime-manifest-sequence.mjs <产品版本>');
}

process.stdout.write(`${releaseSequenceFloor(version)}\n`);
