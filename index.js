// SPDX-License-Identifier: AGPL-3.0-or-later
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

import { ProfileStore } from './host/profiles.js';
import { SessionManager } from './host/session.js';
import { registerTools } from './host/tools.js';
import { InstrumentTools } from './host/instruments.js';
import { registerRoutes } from './host/routes.js';

const pluginRoot = path.dirname(fileURLToPath(import.meta.url));
// DSH_PROFILE_DIR overrides the default ~/.dsh storage path.
const baseDir = process.env.DSH_PROFILE_DIR || path.join(os.homedir(), '.dsh');

export const name = 'ee-fat-whale';
export const inject = ['tools'];

export function apply(ctx, config = {}) {
  const store = new ProfileStore({ pluginRoot, baseDir, config });
  const sessions = new SessionManager({ store, pluginRoot });
  const instruments = new InstrumentTools({ ctx, store, sessions });

  ctx.effect(() => () => {
    instruments.dispose();
    void sessions.closeAll();
  });

  registerTools(ctx, { store, sessions, pluginRoot, instruments });

  ctx.inject(['webServer'], (webCtx) => {
    const dispose = registerRoutes(webCtx, { store, sessions, pluginRoot });
    webCtx.effect(() => dispose);
  });
}
