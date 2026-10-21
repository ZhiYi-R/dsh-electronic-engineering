import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

import { resolveBinary } from './session.js';

const execFileAsync = promisify(execFile);

// Loading several instruments in a row must not re-sweep the LAN each time.
const CACHE_TTL_MS = 5000;
const cache = new Map();

function cacheKey(options) {
  return JSON.stringify(options);
}

function cached(key) {
  const entry = cache.get(key);
  if (!entry) return null;
  if (Date.now() - entry.at > CACHE_TTL_MS) {
    cache.delete(key);
    return null;
  }
  return entry.value;
}

export function clearDiscoveryCache() {
  cache.clear();
}

function discoverArgs(options) {
  const args = ['discover'];
  if (options.usb === false) args.push('--no-usb');
  if (options.lan) {
    args.push('--lan');
    if (options.mdns === false) args.push('--no-mdns');
    if (options.sweep === false) args.push('--no-sweep');
    for (const subnet of options.subnets ?? []) args.push('--subnet', String(subnet));
    for (const host of options.hosts ?? []) args.push('--host', String(host));
    for (const port of options.ports ?? []) args.push('--port', String(port));
    if (options.probeIdn === false) args.push('--no-idn');
  }
  return args;
}

export async function discover({ pluginRoot, store, ...options }) {
  const key = cacheKey(options);
  const hit = cached(key);
  if (hit) return hit;

  const binary = await resolveBinary(pluginRoot);
  const installed = (await store.list()).filter((profile) => profile.valid !== false);
  const requested = { ...options };
  if (requested.lan && !(requested.ports ?? []).length) {
    // Only sweep ports that an installed profile claims to use.
    const ports = new Set();
    for (const profile of installed) {
      for (const port of profile.network?.ports ?? []) {
        const value = Number(port);
        if (Number.isInteger(value) && value > 0 && value < 65536) ports.add(value);
      }
    }
    if (ports.size) requested.ports = [...ports].sort((left, right) => left - right);
  }
  const { stdout } = await execFileAsync(binary, discoverArgs(requested), {
    timeout: 60000,
    maxBuffer: 4 * 1024 * 1024,
  });
  let instruments;
  try {
    instruments = JSON.parse(stdout);
  } catch (error) {
    throw new Error(`invalid discovery output: ${error.message}`);
  }
  if (!Array.isArray(instruments)) {
    throw new Error('invalid discovery output: expected an array');
  }

  for (const instrument of instruments) {
    const match = installed.find((profile) => matchesProfile(profile, instrument));
    if (match) {
      instrument.profile_id = match.id;
      instrument.profile_name = match.name;
    }
  }

  const result = {
    instruments,
    profiles: installed.map((profile) => ({ id: profile.id, name: profile.name })),
  };
  cache.set(key, { at: Date.now(), value: result });
  return result;
}

export function scanOptions(input = {}) {
  const object = input && typeof input === 'object' && !Array.isArray(input) ? input : {};
  return {
    usb: object.usb !== false,
    lan: object.lan !== false,
    mdns: object.mdns !== false,
    sweep: object.sweep !== false,
    subnets: Array.isArray(object.subnets) ? object.subnets.map(String) : [],
    hosts: Array.isArray(object.hosts) ? object.hosts.map(String) : [],
    ports: Array.isArray(object.ports) ? object.ports.map(Number) : [],
    probeIdn: object.probe_idn !== false,
  };
}

// A LAN instrument has only its *IDN? text to be recognised by.
function matchesProfile(profile, instrument) {
  if (profile.usb && instrument.vendor_id !== undefined) {
    const vid = Number.parseInt(profile.usb.vid, 16);
    const pid = Number.parseInt(profile.usb.pid, 16);
    if (
      Number.isFinite(vid) &&
      Number.isFinite(pid) &&
      instrument.vendor_id === vid &&
      instrument.product_id === pid
    ) {
      return true;
    }
  }

  const haystack = [instrument.idn, instrument.model, instrument.product]
    .filter((value) => typeof value === 'string' && value)
    .join(' ')
    .toUpperCase();
  if (!haystack) return false;

  const needles = [...(profile.idn_match ?? []), ...(profile.network?.idn_match ?? [])];
  return needles.some((needle) => haystack.includes(String(needle).toUpperCase()));
}
