import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

import { resolveBinary } from './session.js';

const execFileAsync = promisify(execFile);

// Enumerate USBTMC devices via `scpi discover` and match them against installed profiles.
export async function discover({ pluginRoot, store }) {
  const binary = await resolveBinary(pluginRoot);
  const { stdout } = await execFileAsync(
    binary,
    ['discover'],
    { timeout: 15000, maxBuffer: 2 * 1024 * 1024 },
  );
  let instruments;
  try {
    instruments = JSON.parse(stdout);
  } catch (error) {
    throw new Error(`invalid discovery output: ${error.message}`);
  }
  if (!Array.isArray(instruments)) {
    throw new Error('invalid discovery output: expected an array');
  }
  const profiles = (await store.list()).filter((profile) => profile.valid !== false);
  for (const instrument of instruments) {
    const idn = typeof instrument.idn === 'string' ? instrument.idn.toUpperCase() : null;
    const match = profiles.find((profile) => {
      if (profile.usb) {
        const vid = Number.parseInt(profile.usb.vid, 16);
        const pid = Number.parseInt(profile.usb.pid, 16);
        if (Number.isFinite(vid) && Number.isFinite(pid)) {
          if (instrument.vendor_id === vid && instrument.product_id === pid) return true;
        }
      }
      return Boolean(
        idn &&
          (profile.idn_match ?? []).some((needle) => idn.includes(String(needle).toUpperCase())),
      );
    });
    if (match) {
      instrument.profile_id = match.id;
      instrument.profile_name = match.name;
    }
  }
  return {
    instruments,
    profiles: profiles.map((profile) => ({ id: profile.id, name: profile.name })),
  };
}
