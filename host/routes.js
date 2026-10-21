import { resolveBinary } from './session.js';
import { discover } from './discover.js';

const PREFIX = '/api/ee-fat-whale';
const STATIC_TOOLS = new Set([
  'instrument_profiles',
  'instrument_scan',
  'instrument_load',
  'instrument_unload',
  'instrument_session',
]);

export function registerRoutes(ctx, { store, sessions, pluginRoot }) {
  const disposers = [];
  const route = (path, post, operation) => {
    disposers.push(
      ctx.webServer.register({
        kind: 'exact',
        path,
        handler: async (req, res) => {
          await respond(res, async () => {
            guard(req, { post });
            return operation(req);
          });
        },
      }),
    );
  };

  route(`${PREFIX}/state`, false, () => state(store, sessions, pluginRoot));

  route(`${PREFIX}/health`, false, () => ({
    ok: true,
    tools:
      ctx
        .get('tools')
        ?.schemas()
        .filter((tool) => STATIC_TOOLS.has(tool.name))
        .map((tool) => tool.name) ?? [],
  }));

  route(`${PREFIX}/settings`, true, async (req) => {
    const body = await readJson(req);
    await store.saveSettings(body);
    return state(store, sessions, pluginRoot);
  });

  route(`${PREFIX}/discover`, true, async () => {
    const result = await discover({ pluginRoot, store });
    return { ok: true, ...result };
  });

  route(`${PREFIX}/profiles/import`, true, async (req) => {
    const body = await readJson(req, 12 * 1024 * 1024);
    if (typeof body.filename !== 'string') throw new Error('filename is required');
    if (typeof body.data !== 'string' || !body.data) throw new Error('data is required');
    const buffer = Buffer.from(body.data, 'base64');
    if (!buffer.length) throw new Error('bundle is empty');
    await store.installArchive(buffer, body.filename);
    return state(store, sessions, pluginRoot);
  });

  route(`${PREFIX}/profiles/install`, true, async (req) => {
    const body = await readJson(req);
    await store.install(body.profile, body.lua);
    return state(store, sessions, pluginRoot);
  });

  route(`${PREFIX}/profiles/install-bundled`, true, async (req) => {
    const body = await readJson(req);
    if (typeof body.name !== 'string') throw new Error('name is required');
    await store.installBundled(body.name);
    return state(store, sessions, pluginRoot);
  });

  route(`${PREFIX}/profiles/remove`, true, async (req) => {
    const body = await readJson(req);
    if (typeof body.id !== 'string') throw new Error('id is required');
    await store.remove(body.id, { isActive: (id) => sessions.isProfileActive(id) });
    return state(store, sessions, pluginRoot);
  });

  route(`${PREFIX}/sessions/open`, true, async (req) => {
    const body = await readJson(req);
    if (typeof body.profile !== 'string') throw new Error('profile is required');
    if (typeof body.transport !== 'string') throw new Error('transport is required');
    const session = await sessions.open({
      profileId: body.profile,
      transport: body.transport,
    });
    return { ok: true, session };
  });

  route(`${PREFIX}/sessions/close`, true, async (req) => {
    const body = await readJson(req);
    if (typeof body.session_id !== 'string') throw new Error('session_id is required');
    await sessions.close(body.session_id);
    return state(store, sessions, pluginRoot);
  });

  return () => {
    for (const dispose of disposers) dispose();
  };
}

async function state(store, sessions, pluginRoot) {
  const settings = await store.settings();
  let binaryPath = null;
  let binaryError = null;
  try {
    binaryPath = await resolveBinary(pluginRoot);
  } catch (error) {
    binaryError = error.message;
  }
  return {
    ok: true,
    profileDir: await store.profileDir(),
    settings,
    binaryPath,
    binaryError,
    profiles: await store.list(),
    bundled: await store.listBundled(),
    sessions: sessions.list(),
  };
}

function guard(req, { post }) {
  if (!post) return;
  if (req.method !== 'POST') {
    const error = new Error('POST required');
    error.statusCode = 405;
    throw error;
  }
  const type = String(req.headers['content-type'] ?? '');
  if (!type.includes('application/json')) {
    const error = new Error('application/json required');
    error.statusCode = 415;
    throw error;
  }
  const origin = req.headers.origin;
  if (origin) {
    let host;
    try {
      host = new URL(origin).host;
    } catch {
      const error = new Error('invalid origin');
      error.statusCode = 403;
      throw error;
    }
    if (host !== req.headers.host) {
      const error = new Error('cross-origin request rejected');
      error.statusCode = 403;
      throw error;
    }
  }
}

async function respond(res, operation) {
  try {
    const value = await operation();
    sendJson(res, 200, value);
  } catch (error) {
    if (res.headersSent) return;
    sendJson(res, error.statusCode ?? 400, {
      ok: false,
      error: { message: error.message },
    });
  }
}

function readJson(req, limit = 4 * 1024 * 1024) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    req.on('data', (chunk) => {
      size += chunk.length;
      if (size > limit) {
        const error = new Error(`request body exceeds ${limit} bytes`);
        error.statusCode = 413;
        reject(error);
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on('end', () => {
      const text = Buffer.concat(chunks).toString('utf8');
      if (!text.trim()) return resolve({});
      try {
        resolve(JSON.parse(text));
      } catch (error) {
        error.statusCode = 400;
        reject(new Error(`invalid JSON: ${error.message}`));
      }
    });
    req.on('error', reject);
  });
}

function sendJson(res, status, value) {
  const body = JSON.stringify(value);
  res.statusCode = status;
  res.setHeader('content-type', 'application/json; charset=utf-8');
  res.setHeader('cache-control', 'no-store');
  res.end(body);
}
