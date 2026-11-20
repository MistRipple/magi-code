// Host-owned durable credential, attachment, and KV storage backends for the shared DSH runtime.
import { createHash, randomUUID } from 'node:crypto';
import { createWriteStream, existsSync, mkdirSync } from 'node:fs';
import { mkdir, open, readdir, readFile, rename, rm, stat } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { writeFileAtomic } from '@deepseek-ai/dsh-atomic-write';
import {
  AttachmentError,
  AttachmentId,
  AttachmentStore,
  ImageVariantId,
} from '@deepseek-ai/dsh-attachment';
import {
  CredentialProvider,
  credentialRef,
  isCredentialRefName,
  parseCredentialKey,
} from '@deepseek-ai/dsh-credentials';
import { StorageError, UNIT_NAME_RE } from '@deepseek-ai/dsh-storage';

const IMAGE_LIMITS = Object.freeze({
  maxImageBytes: 20 * 1024 * 1024,
  maxImagesPerMessage: 20,
  maxMessageImageBytes: 100 * 1024 * 1024,
  maxImagePixels: 64 * 1024 * 1024,
  maxImageDimension: 16384,
  mediaTypes: Object.freeze(['image/png', 'image/jpeg', 'image/webp', 'image/gif']),
});

const DIGEST_REF_PATTERN = /^sha256-([0-9a-f]{64})$/;

function sanitizeLeafName(raw, fallback = 'attachment.bin') {
  if (typeof raw !== 'string') return fallback;
  const leaf = basename(raw.replaceAll('\\', '/')).replace(/[\u0000-\u001f/\\]/g, '_').trim();
  if (!leaf || leaf === '.' || leaf === '..') return fallback;
  return leaf.slice(0, 200);
}

function maskSecret(value) {
  if (value.length <= 8) return '••••';
  return `${value.slice(0, 3)}••••${value.slice(-3)}`;
}

function inspectPng(bytes) {
  if (bytes.byteLength < 24) return null;
  if (
    bytes[0] !== 0x89 || bytes[1] !== 0x50 || bytes[2] !== 0x4e || bytes[3] !== 0x47
    || bytes[4] !== 0x0d || bytes[5] !== 0x0a || bytes[6] !== 0x1a || bytes[7] !== 0x0a
  ) return null;
  if (bytes[12] !== 0x49 || bytes[13] !== 0x48 || bytes[14] !== 0x44 || bytes[15] !== 0x52) {
    throw new AttachmentError('PNG is missing a leading IHDR chunk.', 'INVALID_IMAGE');
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const width = view.getUint32(16, false);
  const height = view.getUint32(20, false);
  if (width < 1 || height < 1) throw new AttachmentError('PNG dimensions must be positive.', 'INVALID_IMAGE');
  return { mediaType: 'image/png', width, height };
}

function inspectGif(bytes) {
  if (bytes.byteLength < 10) return null;
  const head = Buffer.from(bytes.subarray(0, 6)).toString('ascii');
  if (head !== 'GIF87a' && head !== 'GIF89a') return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const width = view.getUint16(6, true);
  const height = view.getUint16(8, true);
  if (width < 1 || height < 1) throw new AttachmentError('GIF dimensions must be positive.', 'INVALID_IMAGE');
  return { mediaType: 'image/gif', width, height };
}

function inspectJpeg(bytes) {
  if (bytes.byteLength < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 2;
  while (offset + 4 <= bytes.byteLength) {
    if (bytes[offset] !== 0xff) {
      offset += 1;
      continue;
    }
    while (offset < bytes.byteLength && bytes[offset] === 0xff) offset += 1;
    if (offset >= bytes.byteLength) break;
    const marker = bytes[offset];
    offset += 1;
    if (marker === 0xd9 || marker === 0xda) break;
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue;
    if (offset + 2 > bytes.byteLength) break;
    const segLen = view.getUint16(offset, false);
    if (segLen < 2 || offset + segLen > bytes.byteLength) {
      throw new AttachmentError('JPEG segment length is truncated.', 'INVALID_IMAGE');
    }
    const isSof = (
      (marker >= 0xc0 && marker <= 0xc3)
      || (marker >= 0xc5 && marker <= 0xc7)
      || (marker >= 0xc9 && marker <= 0xcb)
      || (marker >= 0xcd && marker <= 0xcf)
    );
    if (isSof) {
      if (segLen < 7) throw new AttachmentError('JPEG SOF header is truncated.', 'INVALID_IMAGE');
      const height = view.getUint16(offset + 3, false);
      const width = view.getUint16(offset + 5, false);
      if (width < 1 || height < 1) throw new AttachmentError('JPEG dimensions must be positive.', 'INVALID_IMAGE');
      return { mediaType: 'image/jpeg', width, height };
    }
    offset += segLen;
  }
  throw new AttachmentError('JPEG image has no readable frame header.', 'INVALID_IMAGE');
}

function inspectWebp(bytes) {
  if (bytes.byteLength < 30) return null;
  if (
    Buffer.from(bytes.subarray(0, 4)).toString('ascii') !== 'RIFF'
    || Buffer.from(bytes.subarray(8, 12)).toString('ascii') !== 'WEBP'
  ) return null;
  const chunk = Buffer.from(bytes.subarray(12, 16)).toString('ascii');
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let width = 0;
  let height = 0;
  if (chunk === 'VP8X') {
    width = 1 + (bytes[24] | (bytes[25] << 8) | (bytes[26] << 16));
    height = 1 + (bytes[27] | (bytes[28] << 8) | (bytes[29] << 16));
  } else if (chunk === 'VP8 ') {
    if (bytes[23] !== 0x9d || bytes[24] !== 0x01 || bytes[25] !== 0x2a) {
      throw new AttachmentError('WebP VP8 keyframe signature is invalid.', 'INVALID_IMAGE');
    }
    width = view.getUint16(26, true) & 0x3fff;
    height = view.getUint16(28, true) & 0x3fff;
  } else if (chunk === 'VP8L') {
    if (bytes[20] !== 0x2f) throw new AttachmentError('WebP VP8L signature is invalid.', 'INVALID_IMAGE');
    const bits = view.getUint32(21, true);
    width = (bits & 0x3fff) + 1;
    height = ((bits >>> 14) & 0x3fff) + 1;
  } else {
    throw new AttachmentError('Unsupported WebP chunk header.', 'INVALID_IMAGE');
  }
  if (width < 1 || height < 1) throw new AttachmentError('WebP dimensions must be positive.', 'INVALID_IMAGE');
  return { mediaType: 'image/webp', width, height };
}

export function inspectImageBytes(bytes, declaredMediaType) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0) {
    throw new AttachmentError('Image bytes cannot be empty.', 'INVALID_IMAGE');
  }
  if (bytes.byteLength > IMAGE_LIMITS.maxImageBytes) {
    throw new AttachmentError('Image exceeds the single-image byte limit.', 'IMAGE_TOO_LARGE');
  }
  if (!IMAGE_LIMITS.mediaTypes.includes(declaredMediaType)) {
    throw new AttachmentError(`Image type ${declaredMediaType} is not accepted.`, 'UNSUPPORTED_IMAGE_TYPE');
  }
  const detected = inspectPng(bytes) ?? inspectJpeg(bytes) ?? inspectWebp(bytes) ?? inspectGif(bytes);
  if (!detected) {
    throw new AttachmentError('Encoded bytes are not a valid raster image.', 'INVALID_IMAGE');
  }
  if (detected.mediaType !== declaredMediaType) {
    throw new AttachmentError(
      `Declared image type ${declaredMediaType} does not match encoded ${detected.mediaType}.`,
      'IMAGE_TYPE_MISMATCH',
    );
  }
  if (detected.width > IMAGE_LIMITS.maxImageDimension || detected.height > IMAGE_LIMITS.maxImageDimension) {
    throw new AttachmentError('Image dimension exceeds the maximum allowed edge.', 'IMAGE_DIMENSION_TOO_LARGE');
  }
  if (detected.width * detected.height > IMAGE_LIMITS.maxImagePixels) {
    throw new AttachmentError('Image pixel count exceeds the maximum budget.', 'IMAGE_TOO_MANY_PIXELS');
  }
  return detected;
}

function extensionForImage(mediaType) {
  switch (mediaType) {
    case 'image/png': return '.png';
    case 'image/jpeg': return '.jpg';
    case 'image/webp': return '.webp';
    case 'image/gif': return '.gif';
    default: throw new AttachmentError(`Unsupported image mediaType: ${mediaType}`, 'UNSUPPORTED_IMAGE_TYPE');
  }
}

export function createCredentialServiceClass(dshHome) {
  const storePath = join(dshHome, '.credentials.json');
  let queue = Promise.resolve();

  function enqueue(fn) {
    const run = queue.then(fn, fn);
    queue = run.catch(() => {});
    return run;
  }

  async function readDocument() {
    try {
      const raw = await readFile(storePath, 'utf8');
      const parsed = JSON.parse(raw);
      return {
        refs: parsed && typeof parsed.refs === 'object' && parsed.refs !== null && !Array.isArray(parsed.refs)
          ? { ...parsed.refs } : {},
        records: parsed && typeof parsed.records === 'object' && parsed.records !== null && !Array.isArray(parsed.records)
          ? { ...parsed.records } : {},
      };
    } catch (error) {
      if (error?.code === 'ENOENT') return { refs: {}, records: {} };
      throw error;
    }
  }

  async function writeDocument(doc) {
    mkdirSync(dshHome, { recursive: true });
    await writeFileAtomic(storePath, `${JSON.stringify(doc, null, 2)}\n`, { mode: 0o600 });
  }

  return class MagiCredentialProvider extends CredentialProvider {
    async resolve(ref) {
      if (!isCredentialRefName(String(ref ?? ''))) return undefined;
      const key = credentialRef(String(ref));
      const doc = await readDocument();
      const stored = typeof doc.refs[key] === 'string' ? doc.refs[key] : '';
      if (stored.length > 0) return { value: stored, source: 'file' };
      const envVal = process.env[key];
      if (typeof envVal === 'string' && envVal.length > 0) return { value: envVal, source: 'env' };
      return undefined;
    }

    async describe(ref) {
      const key = credentialRef(String(ref));
      const doc = await readDocument();
      const stored = typeof doc.refs[key] === 'string' ? doc.refs[key] : '';
      if (stored.length > 0) {
        return { configured: true, source: 'file', writable: true, masked: maskSecret(stored) };
      }
      const envVal = process.env[key];
      if (typeof envVal === 'string' && envVal.length > 0) {
        return { configured: true, source: 'env', writable: true };
      }
      return { configured: false, writable: true };
    }

    async set(ref, value) {
      const key = credentialRef(String(ref));
      if (typeof value !== 'string' || value.length === 0) {
        throw new TypeError(`credential "${key}" cannot be set to an empty string; use unset()`);
      }
      const changed = await enqueue(async () => {
        const doc = await readDocument();
        if (doc.refs[key] === value) return false;
        doc.refs[key] = value;
        await writeDocument(doc);
        return true;
      });
      if (changed) this.notifyUpdated(key);
    }

    async unset(ref) {
      const key = credentialRef(String(ref));
      const changed = await enqueue(async () => {
        const doc = await readDocument();
        if (!Object.hasOwn(doc.refs, key)) return false;
        delete doc.refs[key];
        await writeDocument(doc);
        return true;
      });
      if (changed) this.notifyUpdated(key);
    }

    async readRecord(key) {
      const canonical = parseCredentialKey(String(key));
      const doc = await readDocument();
      const record = doc.records[canonical];
      return record === undefined ? undefined : structuredClone(record);
    }

    async describeRecord(key) {
      const canonical = parseCredentialKey(String(key));
      const doc = await readDocument();
      const record = doc.records[canonical];
      if (record === undefined) return { configured: false, writable: true };
      return { configured: true, kind: record.kind, writable: true };
    }

    async listRecords() {
      const doc = await readDocument();
      return Object.entries(doc.records).map(([key, record]) => ({
        key: parseCredentialKey(key),
        kind: record.kind,
      }));
    }

    async modifyRecord(key, mutate) {
      const canonical = parseCredentialKey(String(key));
      const { outcome, updated } = await enqueue(async () => {
        const doc = await readDocument();
        const current = doc.records[canonical] === undefined ? undefined : structuredClone(doc.records[canonical]);
        const next = await mutate(current);
        if (next === undefined) {
          return { outcome: current, updated: false };
        }
        const detached = JSON.parse(JSON.stringify(next));
        doc.records[canonical] = detached;
        await writeDocument(doc);
        return { outcome: structuredClone(detached), updated: true };
      });
      if (updated) this.notifyRecordUpdated(canonical);
      return outcome;
    }

    async deleteRecord(key) {
      const canonical = parseCredentialKey(String(key));
      const removed = await enqueue(async () => {
        const doc = await readDocument();
        if (!Object.hasOwn(doc.records, canonical)) return false;
        delete doc.records[canonical];
        await writeDocument(doc);
        return true;
      });
      if (removed) this.notifyRecordUpdated(canonical);
    }
  };
}

export function createAttachmentStoreClass(dshHome) {
  const rootDir = join(dshHome, 'attachments');
  const imagesDir = join(rootDir, 'images');
  const filesDir = join(rootDir, 'files');
  const stagingDir = join(rootDir, 'staging');

  function parseDigest(attachmentId) {
    if (typeof attachmentId !== 'string') {
      throw new AttachmentError('Attachment reference id must be a string.', 'INVALID_ATTACHMENT_REF');
    }
    const match = DIGEST_REF_PATTERN.exec(attachmentId);
    if (!match) {
      throw new AttachmentError('Attachment reference id is malformed.', 'INVALID_ATTACHMENT_REF');
    }
    return match[1];
  }

  function resolveImagePath(ref) {
    if (!ref || typeof ref !== 'object') {
      throw new AttachmentError('Image reference must be an object.', 'INVALID_ATTACHMENT_REF');
    }
    const hex = parseDigest(ref.attachmentId);
    const ext = extensionForImage(ref.mediaType);
    if (
      !Number.isSafeInteger(ref.bytes) || ref.bytes < 1
      || !Number.isSafeInteger(ref.width) || ref.width < 1
      || !Number.isSafeInteger(ref.height) || ref.height < 1
    ) {
      throw new AttachmentError('Image reference metadata is invalid.', 'INVALID_ATTACHMENT_REF');
    }
    return join(imagesDir, hex.slice(0, 2), `${hex}${ext}`);
  }

  function resolveFilePath(ref) {
    if (!ref || typeof ref !== 'object') {
      throw new AttachmentError('File reference must be an object.', 'INVALID_ATTACHMENT_REF');
    }
    const hex = parseDigest(ref.attachmentId);
    if (!Number.isSafeInteger(ref.bytes) || ref.bytes < 0 || typeof ref.name !== 'string' || ref.name.length === 0) {
      throw new AttachmentError('File reference metadata is invalid.', 'INVALID_ATTACHMENT_REF');
    }
    const leaf = sanitizeLeafName(ref.name);
    if (leaf !== ref.name) {
      throw new AttachmentError('File reference name is not canonical.', 'INVALID_ATTACHMENT_REF');
    }
    return join(filesDir, hex.slice(0, 2), hex, leaf);
  }

  return class MagiAttachmentStore extends AttachmentStore {
    get imageLimits() {
      return IMAGE_LIMITS;
    }

    async validateImage(input) {
      inspectImageBytes(input.data, input.mediaType);
    }

    async saveImage(input) {
      const info = inspectImageBytes(input.data, input.mediaType);
      const hex = createHash('sha256').update(input.data).digest('hex');
      const attachmentId = AttachmentId(`sha256-${hex}`);
      const ext = extensionForImage(info.mediaType);
      const bucket = join(imagesDir, hex.slice(0, 2));
      const target = join(bucket, `${hex}${ext}`);
      try {
        mkdirSync(bucket, { recursive: true });
        if (!existsSync(target)) {
          await writeFileAtomic(target, input.data, { mode: 0o444 });
        }
      } catch (cause) {
        throw new AttachmentError('Failed to write normalized image attachment.', 'ATTACHMENT_WRITE_FAILED', { cause });
      }
      const cleanName = input.name === undefined ? undefined : sanitizeLeafName(input.name, `image${ext}`);
      return Object.freeze({
        attachmentId,
        mediaType: info.mediaType,
        bytes: input.data.byteLength,
        width: info.width,
        height: info.height,
        ...(cleanName === undefined ? {} : { name: cleanName }),
      });
    }

    async readImage(ref, signal) {
      signal?.throwIfAborted();
      const path = resolveImagePath(ref);
      let raw;
      try {
        raw = await readFile(path);
      } catch (cause) {
        if (cause?.code === 'ENOENT') {
          throw new AttachmentError('Stored image attachment was not found.', 'ATTACHMENT_NOT_FOUND', { cause });
        }
        throw new AttachmentError('Failed to read stored image attachment.', 'ATTACHMENT_READ_FAILED', { cause });
      }
      signal?.throwIfAborted();
      const bytes = new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength);
      if (bytes.byteLength !== ref.bytes) {
        throw new AttachmentError('Stored image byte length does not match reference.', 'ATTACHMENT_CORRUPT');
      }
      const actualHex = createHash('sha256').update(bytes).digest('hex');
      if (`sha256-${actualHex}` !== String(ref.attachmentId)) {
        throw new AttachmentError('Stored image digest does not match reference.', 'ATTACHMENT_CORRUPT');
      }
      const info = inspectImageBytes(bytes, ref.mediaType);
      if (info.width !== ref.width || info.height !== ref.height) {
        throw new AttachmentError('Stored image dimensions do not match reference.', 'ATTACHMENT_CORRUPT');
      }
      return {
        ref: Object.freeze({
          attachmentId: ref.attachmentId,
          mediaType: ref.mediaType,
          bytes: ref.bytes,
          width: ref.width,
          height: ref.height,
          ...(ref.name === undefined ? {} : { name: ref.name }),
          ...(ref.originalDimensions === undefined ? {} : { originalDimensions: { ...ref.originalDimensions } }),
        }),
        data: bytes,
      };
    }

    imageHostPath(ref) {
      return resolveImagePath(ref);
    }

    async readImageRequest(ref, target, signal) {
      const stored = await this.readImage(ref, signal);
      void target;
      return Object.freeze({
        variantId: ImageVariantId(`${ref.attachmentId}:verbatim`),
        attachment: stored.ref,
        data: stored.data,
        mediaType: stored.ref.mediaType,
        bytes: stored.data.byteLength,
        width: stored.ref.width,
        height: stored.ref.height,
        depth: 'uchar',
        space: 'srgb',
        hasAlpha: stored.ref.mediaType !== 'image/jpeg',
      });
    }

    async saveFile(input) {
      if (!(input?.data instanceof Uint8Array)) {
        throw new AttachmentError('File payload must be a Uint8Array.', 'ATTACHMENT_WRITE_FAILED');
      }
      const hex = createHash('sha256').update(input.data).digest('hex');
      const name = sanitizeLeafName(input.name, 'file.bin');
      const dir = join(filesDir, hex.slice(0, 2), hex);
      const target = join(dir, name);
      try {
        mkdirSync(dir, { recursive: true });
        if (!existsSync(target)) {
          await writeFileAtomic(target, input.data, { mode: 0o444 });
        }
      } catch (cause) {
        throw new AttachmentError('Failed to write verbatim file attachment.', 'ATTACHMENT_WRITE_FAILED', { cause });
      }
      return Object.freeze({
        attachmentId: AttachmentId(`sha256-${hex}`),
        name,
        bytes: input.data.byteLength,
      });
    }

    async saveFileStream(input) {
      input.signal?.throwIfAborted();
      mkdirSync(stagingDir, { recursive: true });
      const tempPath = join(stagingDir, `upload-${randomUUID()}.part`);
      const hash = createHash('sha256');
      let bytes = 0;
      const writer = createWriteStream(tempPath, { mode: 0o600 });
      try {
        for await (const chunk of input.data) {
          input.signal?.throwIfAborted();
          const buf = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk);
          hash.update(buf);
          bytes += buf.byteLength;
          if (!writer.write(buf)) {
            await new Promise((resolve, reject) => {
              writer.once('drain', resolve);
              writer.once('error', reject);
            });
          }
        }
        await new Promise((resolve, reject) => {
          writer.end(err => (err ? reject(err) : resolve()));
        });
        input.signal?.throwIfAborted();
        const hex = hash.digest('hex');
        const name = sanitizeLeafName(input.name, 'file.bin');
        const dir = join(filesDir, hex.slice(0, 2), hex);
        const target = join(dir, name);
        mkdirSync(dir, { recursive: true });
        if (existsSync(target)) {
          await rm(tempPath, { force: true });
        } else {
          await rename(tempPath, target);
        }
        return Object.freeze({
          attachmentId: AttachmentId(`sha256-${hex}`),
          name,
          bytes,
        });
      } catch (cause) {
        writer.destroy();
        await rm(tempPath, { force: true }).catch(() => {});
        if (input.signal?.aborted) throw input.signal.reason;
        throw new AttachmentError('Failed to stream file attachment.', 'ATTACHMENT_WRITE_FAILED', { cause });
      }
    }

    async *readFileStream(ref, signal) {
      signal?.throwIfAborted();
      const path = resolveFilePath(ref);
      let fileStat;
      try {
        fileStat = await stat(path);
      } catch (cause) {
        if (cause?.code === 'ENOENT') {
          throw new AttachmentError('Stored file attachment was not found.', 'ATTACHMENT_NOT_FOUND', { cause });
        }
        throw new AttachmentError('Failed to stat stored file attachment.', 'ATTACHMENT_READ_FAILED', { cause });
      }
      if (!fileStat.isFile() || fileStat.size !== ref.bytes) {
        throw new AttachmentError('Stored file attachment size mismatch.', 'ATTACHMENT_CORRUPT');
      }
      if (ref.bytes === 0) return;
      const handle = await open(path, 'r');
      const stream = handle.createReadStream({ autoClose: true, highWaterMark: 64 * 1024 });
      const hash = createHash('sha256');
      let seen = 0;
      try {
        for await (const chunk of stream) {
          signal?.throwIfAborted();
          const buf = new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
          hash.update(buf);
          seen += buf.byteLength;
          yield buf;
        }
      } catch (cause) {
        if (signal?.aborted) throw signal.reason;
        if (cause instanceof AttachmentError) throw cause;
        throw new AttachmentError('Failed while reading stored file attachment.', 'ATTACHMENT_READ_FAILED', { cause });
      }
      if (seen !== ref.bytes || `sha256-${hash.digest('hex')}` !== String(ref.attachmentId)) {
        throw new AttachmentError('Stored file attachment failed digest verification.', 'ATTACHMENT_CORRUPT');
      }
    }

    async admitEncodedFile(input) {
      if (!input || typeof input.data !== 'string') {
        throw new AttachmentError('Encoded file payload must be a base64 string.', 'ATTACHMENT_INVALID');
      }
      const bytes = Buffer.from(input.data, 'base64');
      if (input.data.length > 0 && bytes.byteLength === 0) {
        throw new AttachmentError('Encoded file payload is not valid base64.', 'ATTACHMENT_INVALID');
      }
      return await this.saveFileStream({
        name: input.name,
        signal: input.signal,
        data: (async function* () {
          if (bytes.byteLength > 0) yield bytes;
        })(),
      });
    }

    fileHostPath(ref) {
      return resolveFilePath(ref);
    }
  };
}

const SAFE_RECORD_KEY_RE = /^[a-zA-Z0-9_-]+$/;

function acceptedVersion(version, descriptor) {
  if (!Number.isInteger(version) || version < 0) return false;
  if (version === descriptor.version) return true;
  return Array.isArray(descriptor.compatibleVersions) && descriptor.compatibleVersions.includes(version);
}

/**
 * Durable atomic JSON storage backend implementing `@deepseek-ai/dsh-storage`
 * `StorageBackend` + `KvFacet` + `KvUnit` over `storageRoot`.
 */
export function createJsonFileStorageBackend(storageRoot) {
  let backendClosed = false;
  const openUnits = new Map();

  function assertBackendOpen() {
    if (backendClosed) throw new StorageError('closed', 'storage backend is closed');
  }

  function validateDescriptor(descriptor) {
    if (!descriptor || typeof descriptor.name !== 'string' || !UNIT_NAME_RE.test(descriptor.name)) {
      throw new StorageError('malformed-medium', `invalid storage unit name "${String(descriptor?.name)}"`);
    }
    if (!Number.isInteger(descriptor.version) || descriptor.version < 0) {
      throw new StorageError('malformed-medium', `invalid storage unit version "${String(descriptor.version)}"`);
    }
    if (!Array.isArray(descriptor.tables)) {
      throw new StorageError('malformed-medium', `invalid tables declaration for unit "${descriptor.name}"`);
    }
    for (const table of descriptor.tables) {
      if (typeof table !== 'string' || !UNIT_NAME_RE.test(table)) {
        throw new StorageError('malformed-medium', `invalid storage table name "${String(table)}"`);
      }
    }
  }

  async function openSingleUnit(descriptor) {
    const filePath = join(storageRoot, `${descriptor.name}.json`);
    const tableSet = new Set(descriptor.tables);
    const tables = Object.fromEntries(descriptor.tables.map(t => [t, Object.create(null)]));
    let globalValue = null;
    let materialized = false;
    let closed = false;

    if (existsSync(filePath)) {
      let parsed;
      try {
        parsed = JSON.parse(await readFile(filePath, 'utf8'));
      } catch (cause) {
        throw new StorageError('malformed-medium', `unit "${descriptor.name}" JSON is corrupt`, { cause });
      }
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        throw new StorageError('malformed-medium', `unit "${descriptor.name}" root must be an object`);
      }
      if (!acceptedVersion(parsed.version, descriptor)) {
        throw new StorageError(
          'version-mismatch',
          `unit "${descriptor.name}" stored version ${String(parsed.version)} is incompatible with ${descriptor.version}`,
        );
      }
      if (parsed.tables !== undefined) {
        if (!parsed.tables || typeof parsed.tables !== 'object' || Array.isArray(parsed.tables)) {
          throw new StorageError('malformed-medium', `unit "${descriptor.name}" tables must be an object`);
        }
        for (const table of descriptor.tables) {
          const rawTable = parsed.tables[table];
          if (rawTable === undefined) continue;
          if (!rawTable || typeof rawTable !== 'object' || Array.isArray(rawTable)) {
            throw new StorageError('malformed-medium', `unit "${descriptor.name}" table "${table}" must be an object`);
          }
          for (const [k, v] of Object.entries(rawTable)) {
            tables[table][k] = v;
          }
        }
      }
      if (descriptor.hasGlobal && parsed.global !== undefined) {
        globalValue = parsed.global;
      }
      materialized = true;
    }

    async function flush() {
      mkdirSync(storageRoot, { recursive: true });
      const payload = {
        version: descriptor.version,
        tables: Object.fromEntries(descriptor.tables.map(t => [t, { ...tables[t] }])),
        ...(descriptor.hasGlobal ? { global: globalValue } : {}),
      };
      await writeFileAtomic(filePath, `${JSON.stringify(payload, null, 2)}\n`, {
        dirMode: 0o700,
        fileMode: 0o600,
      });
      materialized = true;
    }

    function assertOpen(table) {
      if (closed || backendClosed) throw new StorageError('closed', `unit "${descriptor.name}" is closed`);
      if (table !== undefined && !tableSet.has(table)) {
        throw new Error(`unit "${descriptor.name}" has no table "${table}"`);
      }
    }

    return {
      async loadAll() {
        assertOpen();
        const outTables = {};
        for (const table of descriptor.tables) {
          outTables[table] = structuredClone({ ...tables[table] });
        }
        return {
          tables: outTables,
          global: descriptor.hasGlobal ? structuredClone(globalValue) : null,
        };
      },
      async putRecord(table, key, value) {
        assertOpen(table);
        tables[table][key] = structuredClone(value);
        await flush();
      },
      async deleteRecord(table, key) {
        assertOpen(table);
        if (!Object.prototype.hasOwnProperty.call(tables[table], key)) return;
        delete tables[table][key];
        await flush();
      },
      async deleteRecords(table, keys) {
        assertOpen(table);
        let changed = false;
        for (const key of keys) {
          if (Object.prototype.hasOwnProperty.call(tables[table], key)) {
            delete tables[table][key];
            changed = true;
          }
        }
        if (changed) await flush();
      },
      async setGlobal(value) {
        assertOpen();
        if (!descriptor.hasGlobal) {
          throw new Error(`unit "${descriptor.name}" does not declare a global slot`);
        }
        globalValue = structuredClone(value);
        await flush();
      },
      async close() {
        if (closed) return;
        closed = true;
        openUnits.delete(descriptor.name);
        void materialized;
      },
    };
  }

  async function openPerRecordUnit(descriptor) {
    const unitDir = join(storageRoot, descriptor.name);
    const tableSet = new Set(descriptor.tables);
    let closed = false;

    function assertOpen(table) {
      if (closed || backendClosed) throw new StorageError('closed', `unit "${descriptor.name}" is closed`);
      if (table !== undefined && !tableSet.has(table)) {
        throw new Error(`unit "${descriptor.name}" has no table "${table}"`);
      }
    }

    function recordPath(table, key) {
      if (typeof key !== 'string' || !SAFE_RECORD_KEY_RE.test(key)) {
        throw new Error(`unsafe per-record storage key "${String(key)}"`);
      }
      return join(unitDir, table, `${key}.json`);
    }

    return {
      async loadAll() {
        assertOpen();
        const tables = {};
        for (const table of descriptor.tables) {
          tables[table] = {};
          const dir = join(unitDir, table);
          if (!existsSync(dir)) continue;
          const entries = await readdir(dir, { withFileTypes: true });
          for (const entry of entries) {
            if (!entry.isFile() || !entry.name.endsWith('.json')) continue;
            const key = entry.name.slice(0, -5);
            if (!SAFE_RECORD_KEY_RE.test(key)) continue;
            let parsed;
            try {
              parsed = JSON.parse(await readFile(join(dir, entry.name), 'utf8'));
            } catch (cause) {
              throw new StorageError('malformed-medium', `record "${key}" in "${descriptor.name}.${table}" is corrupt`, { cause });
            }
            if (!parsed || typeof parsed !== 'object' || !acceptedVersion(parsed.version, descriptor)) {
              continue;
            }
            tables[table][key] = parsed.value;
          }
        }
        let globalValue = null;
        if (descriptor.hasGlobal) {
          const gPath = join(unitDir, 'global.json');
          if (existsSync(gPath)) {
            let parsed;
            try {
              parsed = JSON.parse(await readFile(gPath, 'utf8'));
            } catch (cause) {
              throw new StorageError('malformed-medium', `global in "${descriptor.name}" is corrupt`, { cause });
            }
            if (parsed && typeof parsed === 'object' && acceptedVersion(parsed.version, descriptor)) {
              globalValue = parsed.value ?? null;
            }
          }
        }
        return { tables, global: globalValue };
      },
      async putRecord(table, key, value) {
        assertOpen(table);
        const path = recordPath(table, key);
        await mkdir(join(unitDir, table), { recursive: true });
        await writeFileAtomic(path, `${JSON.stringify({ version: descriptor.version, value }, null, 2)}\n`, {
          dirMode: 0o700,
          fileMode: 0o600,
        });
      },
      async deleteRecord(table, key) {
        assertOpen(table);
        const path = recordPath(table, key);
        await rm(path, { force: true });
      },
      async deleteRecords(table, keys) {
        assertOpen(table);
        const paths = keys.map(key => recordPath(table, key));
        for (const path of paths) await rm(path, { force: true });
      },
      async backupRecord(table, key) {
        assertOpen(table);
        const src = recordPath(table, key);
        const backupDir = join(unitDir, '.corrupt-backups', table);
        await mkdir(backupDir, { recursive: true });
        const dest = join(backupDir, `${key}.${Date.now()}.${randomUUID().slice(0, 8)}.json`);
        await rename(src, dest);
        return dest;
      },
      async setGlobal(value) {
        assertOpen();
        if (!descriptor.hasGlobal) {
          throw new Error(`unit "${descriptor.name}" does not declare a global slot`);
        }
        await mkdir(unitDir, { recursive: true });
        await writeFileAtomic(
          join(unitDir, 'global.json'),
          `${JSON.stringify({ version: descriptor.version, value }, null, 2)}\n`,
          { dirMode: 0o700, fileMode: 0o600 },
        );
      },
      async close() {
        if (closed) return;
        closed = true;
        openUnits.delete(descriptor.name);
      },
    };
  }

  return {
    kv: {
      async open(descriptor) {
        assertBackendOpen();
        validateDescriptor(descriptor);
        if (openUnits.has(descriptor.name)) {
          throw new StorageError('duplicate-mount', `storage unit "${descriptor.name}" is already open`);
        }
        const unit = descriptor.layout === 'per-record'
          ? await openPerRecordUnit(descriptor)
          : await openSingleUnit(descriptor);
        openUnits.set(descriptor.name, unit);
        return unit;
      },
    },
    async close() {
      if (backendClosed) return;
      backendClosed = true;
      const units = [...openUnits.values()];
      openUnits.clear();
      await Promise.allSettled(units.map(unit => unit.close()));
    },
  };
}
