import { resolveBinary } from './session.js';
import { discover } from './discover.js';

const output = {
  schema: { type: 'object', additionalProperties: true },
  render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
};

function register(ctx, definition) {
  ctx.tools.register({ ...definition, output });
}

export function registerTools(ctx, { store, sessions, pluginRoot, instruments }) {
  register(ctx, {
    name: 'instrument_profiles',
    description:
      'Manage SCPI instrument profiles. Use "list" to see installed and bundled profiles, "install_bundle" with a .tar.gz/.tgz/.zip path to import an instrument atomically, "install_bundled" with name "dm3058" for the bundled Rigol DM3058 package, "install" for an inline scratch profile, or "remove" with a profile id.',
    parameters: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: ['list', 'install', 'install_bundle', 'install_bundled', 'remove'],
          description: 'Operation to perform.',
        },
        name: {
          type: 'string',
          description: 'Bundled instrument name for install_bundled, or profile id for remove.',
        },
        path: {
          type: 'string',
          description: 'Local path to an instrument bundle (.tar.gz, .tgz, or .zip) for install_bundle.',
        },
        profile: {
          oneOf: [
            { type: 'object', additionalProperties: true },
            { type: 'string' },
          ],
          description: 'Inline SCPI profile JSON object (or JSON string) for action "install".',
        },
        lua: {
          type: 'string',
          description: 'Optional Lua macro source for action "install".',
        },
      },
      required: ['action'],
      additionalProperties: false,
    },
    async execute(args) {
      let binary = null;
      let binaryError = null;
      try {
        binary = await resolveBinary(pluginRoot);
      } catch (error) {
        binaryError = error.message;
      }
      let result = {};
      if (args.action === 'install') {
        result = await store.install(args.profile, args.lua);
      } else if (args.action === 'install_bundle') {
        if (!args.path) throw new Error('path is required for install_bundle');
        result = await store.installBundleFile(args.path);
      } else if (args.action === 'install_bundled') {
        if (!args.name) throw new Error('name is required for install_bundled');
        result = await store.installBundled(args.name);
      } else if (args.action === 'remove') {
        if (!args.name) throw new Error('name is required for remove');
        result = await store.remove(args.name, { isActive: (id) => sessions.isProfileActive(id) });
      } else if (args.action !== 'list') {
        throw new Error(`unknown action '${args.action}'`);
      }
      const { profile: _profile, ...installed } = result;
      return {
        ok: true,
        profileDir: await store.profileDir(),
        binaryPath: binary,
        binaryError,
        profiles: await store.list(),
        bundled: await store.listBundled(),
        result: installed,
      };
    },
  });

  register(ctx, {
    name: 'instrument_scan',
    description:
      'Scan USB for USBTMC instruments on Windows, macOS, and Linux. Returns a cross-platform resource "usb:<vid>:<pid>[:<serial>]", USB IDs, serial/manufacturer/product strings, and the installed profile matched by USB VID/PID. Does not open the device or query *IDN?.',
    parameters: {
      type: 'object',
      properties: {},
      additionalProperties: false,
    },
    async execute() {
      const result = await discover({ pluginRoot, store });
      for (const instrument of result.instruments) {
        const loaded = instruments?.findByResource(instrument.resource);
        if (loaded) {
          instrument.loaded = true;
          instrument.loaded_prefix = loaded.toolPrefix;
          instrument.loaded_tools = loaded.tools.length;
          instrument.session_id = loaded.sessionId;
        }
      }
      return { ok: true, ...result };
    },
  });

  register(ctx, {
    name: 'instrument_load',
    description:
      'Level 2 of the basic→instrument→tools tree: open or reuse one instrument session and register one instrument-level tool (no command tools yet). The returned instrument_tool is then called with capabilities:[...] to register level-3 command tools for those scopes. Pass capabilities here to activate immediately; use ["*"] for all. Use instrument_unload to remove the instrument and all its tools.',
    parameters: {
      type: 'object',
      properties: {
        resource: {
          type: 'string',
          description: 'Instrument resource from instrument_scan, for example usb:1ab1:09c4:DM3R251000551.',
        },
        profile_id: {
          type: 'string',
          description: 'Installed profile id. Inferred from the scan match when omitted.',
        },
        transport: {
          type: 'string',
          description: 'Optional transport override; defaults to the resource.',
        },
        capabilities: {
          type: 'array',
          items: { type: 'string' },
          description: 'Optional scope list to activate immediately. Omit to only register the instrument tool; use ["*"] for all commands.',
        },
        scopes: {
          type: 'array',
          items: { type: 'string' },
          description: 'Alias for capabilities.',
        },
      },
      required: ['resource'],
      additionalProperties: false,
    },
    async execute(args) {
      if (!instruments) throw new Error('dynamic instrument tools are unavailable');
      return { ok: true, ...(await instruments.load(args)) };
    },
  });

  register(ctx, {
    name: 'instrument_unload',
    description:
      'Unload all dynamic command tools for an instrument and close its session. Pass the session_id returned by instrument_load, its tool_prefix, or any one command tool name.',
    parameters: {
      type: 'object',
      properties: {
        tool: { type: 'string', description: 'Any command tool name from the loaded instrument.' },
        prefix: { type: 'string', description: 'Tool prefix returned by instrument_load.' },
        session_id: { type: 'string', description: 'Session id returned by instrument_load.' },
      },
      additionalProperties: false,
    },
    async execute(args) {
      if (!instruments) throw new Error('dynamic instrument tools are unavailable');
      return { ok: true, ...(await instruments.unload(args)) };
    },
  });

  register(ctx, {
    name: 'instrument_session',
    description:
      'Open, list, or close SCPI instrument sessions. One session is one Rust SCPI executor process bound to one profile and transport. Open needs a profile id from instrument_profiles and a transport such as "usb:1ab1:09c4", "tcp://host:port", or "usbtmc:/dev/usbtmc0" (mock is for tests). instrument_load is the higher-level path that also registers one tool per available SCPI command. Always close sessions when finished.',
    parameters: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: ['open', 'list', 'close'],
          description: 'Operation to perform.',
        },
        profile: {
          type: 'string',
          description: 'Profile id for action "open".',
        },
        transport: {
          type: 'string',
          description:
            'Transport for action "open": usb:<vid>:<pid>[:<serial>] (cross-platform, from instrument_scan), tcp://host:port, or usbtmc:/dev/usbtmc0 (Linux kernel path). mock/mock:<responses.json> are for tests.',
        },
        session_id: {
          type: 'string',
          description: 'Session id for action "close".',
        },
      },
      required: ['action'],
      additionalProperties: false,
    },
    async execute(args) {
      if (args.action === 'open') {
        if (!args.profile) throw new Error('profile is required for session open');
        if (!args.transport) throw new Error('transport is required for session open');
        const session = await sessions.open({
          profileId: args.profile,
          transport: args.transport,
        });
        return { ok: true, session };
      }
      if (args.action === 'list') {
        return { ok: true, sessions: sessions.list() };
      }
      if (args.action === 'close') {
        if (!args.session_id) throw new Error('session_id is required for session close');
        return { ok: true, ...(await sessions.close(args.session_id)) };
      }
      throw new Error(`unknown action '${args.action}'`);
    },
  });


}

