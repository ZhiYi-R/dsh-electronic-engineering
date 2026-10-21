import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { readBundle } from './archive.js';

const ID_RE = /^[a-z][a-z0-9._-]*$/;
const BUNDLE_EXTENSIONS = ['.tar.gz', '.tgz', '.zip', '.tar'];
const SETTINGS_KEYS = new Set([
  'profileDir',
  'verifyIdn',
  'requestTimeoutMs',
  'maxSessions',
]);

const DEFAULTS = {
  profileDir: '',
  verifyIdn: true,
  requestTimeoutMs: 30000,
  maxSessions: 4,
};

export class ProfileStore {
  constructor({ pluginRoot, baseDir, config = {} }) {
    this.pluginRoot = pluginRoot;
    this.baseDir = baseDir;
    this.bundledDir = path.join(pluginRoot, 'bundles');
    this.settingsFile = path.join(baseDir, 'ee-fat-whale', 'settings.json');
    this.defaults = { ...DEFAULTS, ...pickSettings(config) };
    this.cached = null;
  }

  async load() {
    if (this.cached) return this.cached;
    let saved = {};
    try {
      saved = JSON.parse(await fs.readFile(this.settingsFile, 'utf8'));
    } catch (error) {
      if (error.code !== 'ENOENT') {
        throw new Error(`cannot read settings ${this.settingsFile}: ${error.message}`);
      }
    }
    this.cached = { ...this.defaults, ...pickSettings(saved) };
    return this.cached;
  }

  async settings() {
    return this.load();
  }

  async saveSettings(patch) {
    const current = await this.load();
    const clean = pickSettings(patch);
    const next = { ...current, ...clean };
    if (next.profileDir && typeof next.profileDir !== 'string') {
      throw new Error('profileDir must be a string');
    }
    if (typeof next.verifyIdn !== 'boolean') {
      throw new Error('verifyIdn must be a boolean');
    }
    if (typeof next.maxSessions !== 'number' || next.maxSessions < 1 || next.maxSessions > 32) {
      throw new Error('maxSessions must be between 1 and 32');
    }
    if (
      typeof next.requestTimeoutMs !== 'number' ||
      next.requestTimeoutMs < 1000 ||
      next.requestTimeoutMs > 600000
    ) {
      throw new Error('requestTimeoutMs must be between 1000 and 600000');
    }
    await fs.mkdir(path.dirname(this.settingsFile), { recursive: true });
    await atomicWrite(this.settingsFile, `${JSON.stringify(next, null, 2)}\n`);
    this.cached = next;
    if (next.profileDir) {
      await fs.mkdir(this.expand(next.profileDir), { recursive: true });
    }
    return next;
  }

  expand(value) {
    if (typeof value !== 'string') throw new Error('path must be a string');
    const expanded = value.startsWith('~')
      ? path.join(os.homedir(), value.slice(1))
      : value;
    return path.resolve(expanded);
  }

  async profileDir() {
    const config = await this.load();
    if (config.profileDir) return this.expand(config.profileDir);
    return path.join(this.baseDir, 'ee-fat-whale', 'profiles');
  }

  async ensureProfileDir() {
    const dir = await this.profileDir();
    await fs.mkdir(dir, { recursive: true });
    return dir;
  }

  async list() {
    const dir = await this.ensureProfileDir();
    const entries = await fs.readdir(dir, { withFileTypes: true });
    const profiles = [];
    for (const entry of entries) {
      if (!entry.isFile() || !entry.name.endsWith('.json') || entry.name.endsWith('.mock.json')) {
        continue;
      }
      const file = path.join(dir, entry.name);
      try {
        const profile = JSON.parse(await fs.readFile(file, 'utf8'));
        profiles.push(summary(profile, file, true));
      } catch (error) {
        profiles.push({
          file,
          valid: false,
          error: error.message,
        });
      }
    }
    return profiles.sort((a, b) => String(a.id ?? a.file).localeCompare(String(b.id ?? b.file)));
  }

  async listBundled() {
    let entries;
    try {
      entries = await fs.readdir(this.bundledDir, { withFileTypes: true });
    } catch (error) {
      if (error.code === 'ENOENT') return [];
      throw error;
    }
    const profiles = [];
    for (const entry of entries) {
      if (!entry.isFile() || !BUNDLE_EXTENSIONS.some((ext) => entry.name.endsWith(ext))) {
        continue;
      }
      const file = path.join(this.bundledDir, entry.name);
      try {
        const bundle = readBundle(await fs.readFile(file), entry.name);
        const name = bundleName(entry.name);
        profiles.push({ ...summary(bundle.profile, file, true), bundled_name: name });
      } catch (error) {
        profiles.push({ file, valid: false, error: error.message });
      }
    }
    return profiles.sort((a, b) =>
      String(a.bundled_name ?? a.file).localeCompare(String(b.bundled_name ?? b.file)),
    );
  }

  async get(id) {
    assertId(id);
    const dir = await this.ensureProfileDir();
    const file = path.join(dir, `${id}.json`);
    let profile;
    try {
      profile = JSON.parse(await fs.readFile(file, 'utf8'));
    } catch (error) {
      if (error.code === 'ENOENT') {
        throw new Error(`profile '${id}' is not installed in ${dir}`);
      }
      throw new Error(`cannot read profile '${id}': ${error.message}`);
    }
    validateProfile(profile);
    return { ...summary(profile, file, true), path: file, profile };
  }

  async install(input, lua, context = {}) {
    const profile = typeof input === 'string' ? JSON.parse(input) : structuredClone(input);
    validateProfile(profile);
    const dir = await this.ensureProfileDir();
    const file = path.join(dir, `${profile.id}.json`);

    if (lua !== undefined) {
      if (typeof lua !== 'string') throw new Error('lua must be a string');
      const luaName = `${profile.id}.lua`;
      await atomicWrite(path.join(dir, luaName), lua);
      profile.lua = luaName;
    }
    if (profile.lua !== undefined) {
      if (typeof profile.lua !== 'string' || path.basename(profile.lua) !== profile.lua) {
        throw new Error('profile.lua must be a plain file name (no directories)');
      }
      if (!(await exists(path.join(dir, profile.lua)))) {
        throw new Error(`profile references lua file '${profile.lua}' but no lua source was provided`);
      }
    }

    await atomicWrite(file, `${JSON.stringify(profile, null, 2)}\n`);
    return { ...summary(profile, file, true), path: file, profile, source: context.source };
  }

  async installArchive(buffer, filename) {
    if (!Buffer.isBuffer(buffer)) throw new Error('instrument bundle must be a Buffer');
    const bundle = readBundle(buffer, filename);
    return this.install(bundle.profile, bundle.lua, { source: filename });
  }

  async installBundleFile(filePath) {
    const filename = path.basename(filePath);
    const buffer = await fs.readFile(filePath);
    return this.installArchive(buffer, filename);
  }

  async installBundled(name) {
    if (typeof name !== 'string' || !/^[a-z0-9._-]+$/.test(name)) {
      throw new Error('bundled profile name is invalid');
    }
    const file = await this.bundleFile(name);
    const buffer = await fs.readFile(file);
    return this.installArchive(buffer, path.basename(file));
  }

  async bundleFile(name) {
    for (const extension of BUNDLE_EXTENSIONS) {
      const file = path.join(this.bundledDir, `${name}${extension}`);
      if (await exists(file)) return file;
    }
    throw new Error(`bundled instrument '${name}' does not exist`);
  }

  async remove(id, { isActive } = {}) {
    assertId(id);
    if (isActive?.(id)) {
      throw new Error(`profile '${id}' is used by an open session; close the session first`);
    }
    const dir = await this.ensureProfileDir();
    const file = path.join(dir, `${id}.json`);
    let profile;
    try {
      profile = JSON.parse(await fs.readFile(file, 'utf8'));
    } catch (error) {
      if (error.code === 'ENOENT') throw new Error(`profile '${id}' is not installed`);
      throw error;
    }
    await fs.unlink(file);
    if (profile.lua && path.basename(profile.lua) === profile.lua) {
      await fs.unlink(path.join(dir, profile.lua)).catch(() => {});
    }
    await fs.unlink(path.join(dir, `${id}.mock.json`)).catch(() => {});
    return { id, removed: true };
  }
}

function pickSettings(value) {
  const out = {};
  if (!value || typeof value !== 'object') return out;
  for (const [key, candidate] of Object.entries(value)) {
    if (SETTINGS_KEYS.has(key) && candidate !== undefined) out[key] = candidate;
  }
  return out;
}

function bundleName(filename) {
  for (const extension of BUNDLE_EXTENSIONS) {
    if (filename.endsWith(extension)) return filename.slice(0, -extension.length);
  }
  return filename;
}

function assertId(id) {
  if (typeof id !== 'string' || !ID_RE.test(id)) {
    throw new Error('profile id is invalid');
  }
}

function validateProfile(profile) {
  if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
    throw new Error('profile must be a JSON object');
  }
  if (profile.schema_version !== 1) {
    throw new Error('profile.schema_version must be 1');
  }
  if (typeof profile.id !== 'string' || !ID_RE.test(profile.id)) {
    throw new Error('profile.id must match [a-z][a-z0-9._-]*');
  }
  if (typeof profile.name !== 'string' || !profile.name.trim()) {
    throw new Error('profile.name is required');
  }
  if (!Array.isArray(profile.commands) || profile.commands.length === 0) {
    throw new Error('profile.commands must be a non-empty array');
  }
  if (profile.lua !== undefined && typeof profile.lua !== 'string') {
    throw new Error('profile.lua must be a string');
  }
  return profile;
}

function summary(profile, file, valid) {
  return {
    id: profile.id,
    name: profile.name,
    manufacturer: profile.manufacturer,
    description: profile.description,
    commands: Array.isArray(profile.commands) ? profile.commands.length : 0,
    channels: Number.isInteger(profile.channels) ? profile.channels : null,
    tool_name: profile.tool_name ?? null,
    capabilities: Array.isArray(profile.capabilities)
      ? profile.capabilities.map((cap) => ({
          id: cap.id,
          name: cap.name,
          description: cap.description,
          match: Array.isArray(cap.match) ? cap.match : [],
        }))
      : [],
    default_capabilities: Array.isArray(profile.default_capabilities)
      ? profile.default_capabilities
      : [],
    idn_match: Array.isArray(profile.idn_match) ? profile.idn_match : [],
    usb: profile.usb ?? null,
    file,
    valid,
  };
}

async function exists(file) {
  try {
    await fs.access(file);
    return true;
  } catch {
    return false;
  }
}

async function atomicWrite(file, content) {
  const temporary = `${file}.${process.pid}.${Date.now()}.tmp`;
  await fs.mkdir(path.dirname(file), { recursive: true });
  try {
    await fs.writeFile(temporary, content, 'utf8');
    await fs.rename(temporary, file);
  } catch (error) {
    await fs.unlink(temporary).catch(() => {});
    throw error;
  }
}
