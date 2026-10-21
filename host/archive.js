import path from 'node:path';
import zlib from 'node:zlib';

const MAX_ENTRIES = 64;
const MAX_UNPACKED = 16 * 1024 * 1024;

/**
 * 安全解析仪器归档包（.tar.gz / .tgz / .zip / .tar）：
 * 1. 防解压炸弹：限制文件项最多 64 个，解压后总大小上限 16MB；
 * 2. 格式完整性约束：包内必须包含且仅包含一个 Profile JSON，可附带一个配套 Lua 脚本；
 * 3. 拒绝任意未知多余文件，防范恶意脚本夹带；
 * 4. 彻底防范目录穿越（Path Traversal）与符号链接劫持。
 */
export function readBundle(buffer, filename) {
  const entries = extract(buffer, filename);
  const files = flatten(entries);
  const jsonFiles = files.filter(([name]) => name.toLowerCase().endsWith('.json'));
  if (jsonFiles.length !== 1) {
    throw new Error('instrument bundle must contain exactly one profile JSON');
  }
  const luaFiles = files.filter(([name]) => name.toLowerCase().endsWith('.lua'));
  const unknown = files.filter(
    ([name]) => !name.toLowerCase().endsWith('.json') && !name.toLowerCase().endsWith('.lua'),
  );
  if (unknown.length) {
    throw new Error(`unsupported file in instrument bundle: ${unknown[0][0]}`);
  }

  let profile;
  try {
    profile = JSON.parse(jsonFiles[0][1].toString('utf8'));
  } catch (error) {
    throw new Error(`invalid profile JSON: ${error.message}`);
  }

  let lua;
  if (typeof profile.lua === 'string') {
    const reference = path.basename(profile.lua);
    const entry = files.find(([name]) => name === reference);
    if (!entry) {
      throw new Error(`bundle is missing Lua file '${reference}'`);
    }
    lua = entry[1].toString('utf8');
  } else if (luaFiles.length === 1) {
    lua = luaFiles[0][1].toString('utf8');
  } else if (luaFiles.length > 1) {
    throw new Error('bundle contains multiple Lua files but no profile.lua reference');
  }

  return { profile, lua, filename };
}

function extract(buffer, filename) {
  const lower = String(filename).toLowerCase();
  if (lower.endsWith('.tar.gz') || lower.endsWith('.tgz')) {
    return extractTar(zlib.gunzipSync(buffer, { maxOutputLength: MAX_UNPACKED }));
  }
  if (lower.endsWith('.tar')) {
    return extractTar(buffer);
  }
  if (lower.endsWith('.zip')) {
    return extractZip(buffer);
  }
  throw new Error('unsupported instrument bundle format (use .tar.gz, .tgz, .zip, or .tar)');
}

function extractTar(buffer) {
  const entries = [];
  let offset = 0;
  let longName = null;
  let paxName = null;
  let total = 0;

  while (offset + 512 <= buffer.length) {
    const header = buffer.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    if (entries.length >= MAX_ENTRIES) throw new Error('instrument bundle has too many files');

    let name = header.toString('utf8', 0, 100).replace(/\0.*$/, '');
    const prefix = header.toString('utf8', 345, 500).replace(/\0.*$/, '');
    if (prefix) name = `${prefix}/${name}`;
    if (paxName) {
      name = paxName;
      paxName = null;
    }
    if (longName) {
      name = longName;
      longName = null;
    }

    const size = parseOctal(header, 124, 12);
    const type = String.fromCharCode(header[156] || 48);
    const dataStart = offset + 512;
    const dataEnd = dataStart + size;
    if (dataEnd > buffer.length) throw new Error('truncated tar entry');
    const data = buffer.subarray(dataStart, dataEnd);

    if (type === 'L') {
      longName = data.toString('utf8').replace(/\0.*$/, '');
    } else if (type === 'x') {
      paxName = parsePaxName(data) ?? paxName;
    } else if (type === '5') {
      // 目录节点：tar 中普通目录不单独生成文件项，直接跳过
    } else if (type === '0' || type === '\0') {
      total += size;
      if (total > MAX_UNPACKED) throw new Error('instrument bundle is too large');
      addEntry(entries, name, data);
    } else if (type === '1' || type === '2') {
      throw new Error('instrument bundle may not contain links');
    } else if (type !== 'g') {
      throw new Error(`unsupported tar entry type '${type}'`);
    }

    offset = dataStart + Math.ceil(size / 512) * 512;
  }
  return entries;
}

function parseOctal(buffer, start, length) {
  const value = buffer
    .toString('ascii', start, start + length)
    .replace(/\0.*$/, '')
    .trim();
  if (!value) return 0;
  const parsed = Number.parseInt(value, 8);
  if (!Number.isFinite(parsed) || parsed < 0) throw new Error('invalid tar size');
  return parsed;
}

function parsePaxName(data) {
  let offset = 0;
  while (offset < data.length) {
    const space = data.indexOf(0x20, offset);
    if (space < 0) break;
    const length = Number.parseInt(data.toString('ascii', offset, space), 10);
    if (!Number.isFinite(length) || length <= 0) break;
    const record = data.subarray(offset, offset + length).toString('utf8');
    const newline = record.indexOf('\n');
    const body = newline >= 0 ? record.slice(0, newline) : record;
    const equals = body.indexOf('=');
    if (equals < 0) break;
    const key = body.slice(body.indexOf(' ') + 1, equals);
    if (key === 'path') return body.slice(equals + 1);
    offset += length;
  }
  return null;
}

function extractZip(buffer) {
  const eocd = findEndOfCentralDirectory(buffer);
  const count = buffer.readUInt16LE(eocd + 10);
  const directoryOffset = buffer.readUInt32LE(eocd + 16);
  if (count === 0xffff || directoryOffset === 0xffffffff) {
    throw new Error('ZIP64 instrument bundles are not supported');
  }
  if (count > MAX_ENTRIES) throw new Error('instrument bundle has too many files');

  const entries = [];
  let offset = directoryOffset;
  let total = 0;
  for (let index = 0; index < count; index += 1) {
    if (buffer.readUInt32LE(offset) !== 0x02014b50) {
      throw new Error('invalid ZIP central directory');
    }
    const flags = buffer.readUInt16LE(offset + 8);
    const method = buffer.readUInt16LE(offset + 10);
    const compressedSize = buffer.readUInt32LE(offset + 20);
    const uncompressedSize = buffer.readUInt32LE(offset + 24);
    const nameLength = buffer.readUInt16LE(offset + 28);
    const extraLength = buffer.readUInt16LE(offset + 30);
    const commentLength = buffer.readUInt16LE(offset + 32);
    const attributes = buffer.readUInt32LE(offset + 38);
    const localOffset = buffer.readUInt32LE(offset + 42);
    const name = buffer.toString('utf8', offset + 46, offset + 46 + nameLength);

    if (flags & 0x0001) throw new Error('encrypted ZIP entries are not supported');
    if (method !== 0 && method !== 8) throw new Error(`unsupported ZIP compression method ${method}`);
    if (((attributes >>> 16) & 0xf000) === 0xa000) {
      throw new Error('instrument bundle may not contain symlinks');
    }
    if (!name.endsWith('/')) {
      if (buffer.readUInt32LE(localOffset) !== 0x04034b50) {
        throw new Error('invalid ZIP local header');
      }
      const localNameLength = buffer.readUInt16LE(localOffset + 26);
      const localExtraLength = buffer.readUInt16LE(localOffset + 28);
      const dataStart = localOffset + 30 + localNameLength + localExtraLength;
      const compressed = buffer.subarray(dataStart, dataStart + compressedSize);
      if (compressed.length !== compressedSize) throw new Error('truncated ZIP entry');
      let data;
      if (method === 0) {
        data = compressed;
      } else {
        data = zlib.inflateRawSync(compressed, { maxOutputLength: MAX_UNPACKED });
      }
      if (data.length !== uncompressedSize) throw new Error('invalid ZIP entry size');
      total += data.length;
      if (total > MAX_UNPACKED) throw new Error('instrument bundle is too large');
      addEntry(entries, name, data);
    }

    offset += 46 + nameLength + extraLength + commentLength;
  }
  return entries;
}

function findEndOfCentralDirectory(buffer) {
  const minimum = Math.max(0, buffer.length - 0xffff - 22);
  for (let offset = buffer.length - 22; offset >= minimum; offset -= 1) {
    if (buffer.readUInt32LE(offset) === 0x06054b50) return offset;
  }
  throw new Error('invalid ZIP: end of central directory not found');
}

function addEntry(entries, rawName, data) {
  const name = normalizeName(rawName);
  if (!name) return;
  if (entries.some(([existing]) => existing === name)) {
    throw new Error(`duplicate entry in instrument bundle: ${name}`);
  }
  entries.push([name, data]);
}

function normalizeName(rawName) {
  let name = String(rawName).replace(/\\/g, '/');
  while (name.startsWith('./')) name = name.slice(2);
  if (name.startsWith('/') || name.split('/').includes('..')) {
    throw new Error(`unsafe path in instrument bundle: ${rawName}`);
  }
  if (!name || name.endsWith('/')) return null;
  return name;
}

function flatten(entries) {
  if (!entries.length) throw new Error('instrument bundle is empty');
  const parts = entries.map(([name]) => name.split('/'));
  const common = parts[0][0];
  if (parts.every((segments) => segments.length > 1 && segments[0] === common)) {
    const stripped = entries.map(([, data], index) => [parts[index].slice(1).join('/'), data]);
    if (stripped.some(([name]) => name.includes('/'))) {
      throw new Error('instrument bundle must be flat or contain a single top-level directory');
    }
    return stripped;
  }
  if (parts.some((segments) => segments.length > 1)) {
    throw new Error('instrument bundle must be flat or contain a single top-level directory');
  }
  return entries;
}
