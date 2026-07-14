import fs from 'node:fs';
import path from 'node:path';

function requiredString(value, name) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${name} is required`);
  }
  return value.trim();
}

function releaseAssetUrl(repository, tag, filename) {
  return `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(filename)}`;
}

export function createUpdateManifest({ version, tag, repository, pubDate, notes, platforms }) {
  const normalizedVersion = requiredString(version, 'version').replace(/^v/, '');
  const normalizedTag = requiredString(tag || `v${normalizedVersion}`, 'tag');
  const normalizedRepository = requiredString(repository, 'repository');
  if (!Array.isArray(platforms) || platforms.length === 0) {
    throw new Error('at least one update platform is required');
  }

  const manifestPlatforms = {};
  for (const platform of platforms) {
    const target = requiredString(platform?.target, 'platform target');
    const filename = requiredString(platform?.filename, `filename for ${target}`);
    const signature = requiredString(platform?.signature, `signature is required for ${target}`);
    if (manifestPlatforms[target]) {
      throw new Error(`duplicate update platform: ${target}`);
    }
    const entry = {
      url: releaseAssetUrl(normalizedRepository, normalizedTag, filename),
      signature,
    };
    manifestPlatforms[target] = entry;

    const targetParts = target.split('-');
    if (targetParts.length === 3) {
      const fallbackTarget = targetParts.slice(0, 2).join('-');
      if (!manifestPlatforms[fallbackTarget]) {
        manifestPlatforms[fallbackTarget] = entry;
      }
    }
  }

  return {
    version: normalizedVersion,
    notes: typeof notes === 'string' ? notes : '',
    pub_date: pubDate || new Date().toISOString(),
    platforms: manifestPlatforms,
  };
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (!token.startsWith('--')) {
      throw new Error(`unexpected argument: ${token}`);
    }
    const name = token.slice(2);
    const value = argv[index + 1];
    if (!value || value.startsWith('--')) {
      throw new Error(`missing value for --${name}`);
    }
    args[name] = value;
    index += 1;
  }
  return args;
}

function scanUpdaterAssets(assetsDir, version) {
  const escapedVersion = version.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const pattern = new RegExp(`^Magi_${escapedVersion}_(?<target>(?:darwin|linux|windows)-[a-z0-9_]+-[a-z0-9_]+)\\.(?:tar\\.gz|AppImage|exe)$`);
  const files = fs.readdirSync(assetsDir).filter((filename) => pattern.test(filename));
  if (files.length === 0) {
    throw new Error(`no normalized updater assets found in ${assetsDir}`);
  }

  return files.map((filename) => {
    const match = pattern.exec(filename);
    const signaturePath = path.join(assetsDir, `${filename}.sig`);
    if (!fs.existsSync(signaturePath)) {
      throw new Error(`signature is missing for ${filename}`);
    }
    return {
      target: match.groups.target,
      filename,
      signature: fs.readFileSync(signaturePath, 'utf8').trim(),
    };
  });
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const version = requiredString(args.version, 'version').replace(/^v/, '');
  const notes = args['notes-file'] ? fs.readFileSync(args['notes-file'], 'utf8') : '';
  const manifest = createUpdateManifest({
    version,
    tag: args.tag,
    repository: args.repository,
    pubDate: args['pub-date'],
    notes,
    platforms: scanUpdaterAssets(args['assets-dir'], version),
  });
  fs.mkdirSync(path.dirname(args.output), { recursive: true });
  fs.writeFileSync(args.output, `${JSON.stringify(manifest, null, 2)}\n`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
