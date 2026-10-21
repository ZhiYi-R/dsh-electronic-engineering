import { discover } from './discover.js';

const output = {
  schema: { type: 'object', additionalProperties: true },
  render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
};

/**
 * 动态仪器工具管理器：
 * 负责实现三层工具模型的第 2 层（单台仪器概览）与第 3 层（具体命令工具），
 * 处理硬件选件探测、能力分组过滤、通道折叠以及输入预校验。
 */
export class InstrumentTools {
  constructor({ ctx, store, sessions }) {
    this.ctx = ctx;
    this.store = store;
    this.sessions = sessions;
    this.loaded = new Map();
    this.options = new Map();
    this.unsubscribe = sessions.onClosed((id) => this.handleClosed(id));
  }

  dispose() {
    this.unsubscribe?.();
    this.unsubscribe = null;
    for (const entry of this.loaded.values()) this.disposeEntry(entry);
    this.loaded.clear();
    this.options.clear();
  }

  handleClosed(id) {
    const entry = this.loaded.get(id);
    if (!entry) return;
    this.disposeEntry(entry);
    this.loaded.delete(id);
    this.options.delete(id);
  }

  disposeEntry(entry) {
    if (entry.disposed) return;
    entry.disposed = true;
    try {
      entry.instrumentTool?.disposer();
    } catch {
      // 宿主插件作用域可能已提前销毁，工具注销失败属于正常现象，安全忽略
    }
    for (const tool of entry.tools) {
      try {
        tool.disposer();
      } catch {
        // 同样忽略已被宿主框架清理的具象工具注销异常
      }
    }
  }

  findByResource(resource) {
    for (const entry of this.loaded.values()) {
      if (entry.transport === resource) return entry;
    }
    return null;
  }

  list() {
    return [...this.loaded.values()].map((entry) => this.describe(entry));
  }

  describe(entry) {
    const session = this.sessions.list().find((item) => item.session_id === entry.sessionId);
    const { commands: _profileCommands, ...publicSession } = session ?? {};
    return {
      session: publicSession,
      profile_command_count: entry.options.profile.commands.length,
      profile_id: entry.profileId,
      profile_name: entry.profileName,
      transport: entry.transport,
      instrument_tool: entry.instrumentTool?.name ?? null,
      channels: entry.channels,
      operation_count: entry.logical.length,
      command_count: entry.tools.length,
      tool_prefix: entry.toolPrefix,
      tool_count: entry.tools.length,
      tools_preview: entry.tools.slice(0, 12).map((tool) => tool.name),
      capabilities: entry.capabilities,
      selected_capabilities: entry.selectedCapabilities,
      default_capabilities: entry.defaultCapabilities,
      options: {
        installed: entry.options.installed,
        known: entry.options.known,
        errors: entry.options.errors,
        features: (entry.options.profile.options?.features ?? []).map((feature) => ({
          id: feature.id,
          name: feature.name,
          requires: feature.requires ?? [],
          available:
            entry.options.known &&
            (feature.requires ?? []).every((option) => entry.options.installed.includes(option)),
        })),
      },
      gated_commands: entry.gatedCommands,
    };
  }

  async load({ resource, profileId, profile_id, transport, capabilities, scopes }) {
    const source = String(resource ?? '').trim();
    if (!source) throw new Error('resource is required');
    const spec = String(transport ?? source).trim();
    if (!spec) throw new Error('transport is required');

    let targetProfile = String(profileId ?? profile_id ?? '').trim();
    if (!targetProfile) {
      const scan = await discover({ pluginRoot: this.store.pluginRoot, store: this.store });
      const found = scan.instruments.find((instrument) => instrument.resource === source);
      if (!found) throw new Error(`no instrument found at ${source}`);
      targetProfile = found.profile_id;
      if (!targetProfile) {
        throw new Error(`no installed profile matches ${source}; install an instrument bundle first`);
      }
    }

    const open = this.sessions
      .list()
      .find((session) => session.profile_id === targetProfile && session.transport === spec);
    const info = open ?? (await this.sessions.open({ profileId: targetProfile, transport: spec }));

    let profile;
    try {
      profile = (await this.store.get(targetProfile)).profile;
    } catch (error) {
      if (!open) await this.sessions.close(info.session_id).catch(() => {});
      throw error;
    }

    const requested = capabilities ?? scopes;
    try {
      const options = await this.ensureOptions(info.session_id, profile);
      const existing = [...this.loaded.values()].find(
        (entry) => entry.profileId === targetProfile && entry.transport === spec,
      );
      if (existing) {
        if (requested !== undefined) await this.activate(existing, requested);
        return this.describe(existing);
      }
      const entry = this.registerInstrument({ info, profile, transport: spec, options });
      if (requested !== undefined) await this.activate(entry, requested);
      return this.describe(entry);
    } catch (error) {
      if (!open) await this.sessions.close(info.session_id).catch(() => {});
      throw error;
    }
  }

  registerInstrument({ info, profile, transport, options }) {
    const prepared = buildLogicalTools(profile, options);
    const serial = transport.startsWith('usb:')
      ? transport.split(':').pop()
      : serialFromIdn(info.idn) || String(info.session_id).replace(/^scpi[_-]?/i, '');
    const model = profile.tool_name || modelFromIdn(info.idn, profile.name);
    const toolPrefix = `instrument_${slug(model)}_${slug(serial)}_`;
    const instrumentName = toolPrefix.slice(0, -1);
    const entry = {
      sessionId: info.session_id,
      profileId: profile.id,
      profileName: profile.name,
      transport,
      toolPrefix,
      instrumentTool: null,
      tools: [],
      logical: prepared.tools,
      channels: prepared.channels,
      options,
      gatedCommands: prepared.gatedCommands,
      selectedCapabilities: [],
      defaultCapabilities: Array.isArray(profile.default_capabilities)
        ? [...profile.default_capabilities]
        : [],
      capabilities: capabilitySummaries(
        profile,
        prepared.availableByCapability,
        [],
      ),
      disposed: false,
    };

    try {
      const definition = this.instrumentDefinition({ entry, instrumentName });
      const disposer = this.ctx.tools.register(definition);
      entry.instrumentTool = { name: instrumentName, disposer };
    } catch (error) {
      this.disposeEntry(entry);
      throw error;
    }

    this.loaded.set(info.session_id, entry);
    return entry;
  }

  instrumentDefinition({ entry, instrumentName }) {
    const instruments = this;
    const capabilities = entry.capabilities
      .map((cap) => `${cap.id} (${cap.tool_count})`)
      .join(', ');
    const defaultText = entry.defaultCapabilities.length
      ? ` Suggested scopes: ${entry.defaultCapabilities.join(', ')}.`
      : '';
    return {
      name: instrumentName,
      description:
        `Instrument-level tool for ${entry.profileName} at ${entry.transport}. ` +
        `Capabilities: ${capabilities}. ${entry.channels} analog channel(s).${defaultText} ` +
        'Call with no arguments to list capabilities; pass capabilities:[...] to register the level-3 command tools for those scopes (["*"] for all).',
      parameters: {
        type: 'object',
        properties: {
          capabilities: {
            type: 'array',
            items: { type: 'string' },
            description: 'Capability scopes to activate. Omit to list; use ["*"] for all.',
          },
        },
        additionalProperties: false,
      },
      output,
      async execute(input) {
        const object = input && typeof input === 'object' && !Array.isArray(input) ? input : {};
        if (object.capabilities === undefined) {
          return {
            ok: true,
            instrument_tool: instrumentName,
            session_id: entry.sessionId,
            channels: entry.channels,
            capabilities: entry.capabilities,
            selected_capabilities: entry.selectedCapabilities,
            default_capabilities: entry.defaultCapabilities,
            tool_count: entry.tools.length,
          };
        }
        return instruments.activate(entry, object.capabilities);
      },
    };
  }

  /**
   * 动态激活或切换第三层命令工具：
   * 1. 注销之前已注册的旧命令工具；
   * 2. 根据 requested 列表（或 ["*"]）筛选匹配的逻辑工具；
   * 3. 逐一注册新的工具到宿主上下文；
   * 4. 出现异常时自动回滚清理，防止残留悬空工具。
   */
  async activate(entry, requested) {
    const selected = resolveCapabilities(entry.options.profile, requested);
    for (const tool of entry.tools) {
      try {
        tool.disposer();
      } catch {
        // 忽略可能已失效的历史工具注销错误
      }
    }
    entry.tools = [];
    entry.selectedCapabilities = selected;
    const wanted = new Set(selected);
    try {
      for (const logical of entry.logical) {
        if (!wanted.has(logical.capability)) continue;
        const name = this.uniqueName(`${entry.toolPrefix}${slug(logical.id)}`.slice(0, 64));
        const definition = this.logicalDefinition({ entry, logical, toolName: name });
        const disposer = this.ctx.tools.register(definition);
        entry.tools.push({ id: logical.id, name, disposer, logical });
      }
    } catch (error) {
      // 注册过程中若有任何一个工具报错，全部回滚以保证状态一致性
      for (const tool of entry.tools) {
        try {
          tool.disposer();
        } catch {
          // 回滚清理过程静默忽略二次错误
        }
      }
      entry.tools = [];
      throw error;
    }
    entry.capabilities = capabilitySummaries(
      entry.options.profile,
      availableCounts(entry.logical),
      selected,
    );
    return { ok: true, ...this.describe(entry) };
  }

  logicalDefinition({ entry, logical, toolName }) {
    const command = logical.command;
    const args = Array.isArray(logical.args) ? logical.args : [];
    const properties = {};
    const required = [];
    const selectorArgs = logical.channelArg ? ['channel'] : [];
    const argsRequired = !(command.readable && command.writable);
    const argText = [];
    for (const arg of args) {
      if (!arg || typeof arg.name !== 'string' || !arg.name) continue;
      properties[arg.name] = this.argumentSchema(arg);
      if (argsRequired || selectorArgs.includes(arg.name)) required.push(arg.name);
      argText.push(`${arg.name}:${arg.type ?? 'any'}`);
    }
    const modes = [];
    if (command.writable) modes.push('writes');
    if (command.readable) modes.push('reads');
    if (!modes.length) modes.push('no I/O');
    const summary = command.summary ? ` ${command.summary}` : '';
    const optionText = Array.isArray(command.requires) && command.requires.length
      ? ` Requires option(s): ${command.requires.join(', ')}.`
      : '';
    const safetyText = command.safety_output
      ? logical.channelArg
        ? ' Safety: the selected channel output is toggled by the Rust executor as required.'
        : ` Safety: output ${command.safety_output} is turned off before the write and on after success.`
      : '';
    const channelText = logical.channelArg
      ? ' The channel argument selects the physical channel; it is required for reads and writes.'
      : entry.channels === 1
        ? ' Single-channel instrument: no channel argument.'
        : '';
    const invocation = command.readable && command.writable
      ? (logical.channelArg
          ? ' Pass channel to query; pass channel and all payload arguments to write.'
          : args.length
            ? ' Call with no arguments to query, or all arguments to write.'
            : ' Call with no arguments to query.')
      : command.writable
        ? ' All arguments are required for a write.'
        : ' Query-only.';
    const description =
      `${entry.profileName} [${logical.capability}] ${logical.id}:${summary} SCPI header ${command.header}; ` +
      `${modes.join('/')}${argText.length ? `; args ${argText.join(', ')}` : '; no arguments'}.` +
      `${channelText}${invocation}${optionText}${safetyText}`;
    const sessions = this.sessions;
    const instruments = this;

    return {
      name: toolName,
      description,
      parameters: {
        type: 'object',
        properties,
        required,
        additionalProperties: false,
      },
      output,
      async execute(input) {
        const values = validateInput(
          { ...logical, id: logical.id, selectorArgs },
          input,
        );
        let channel = null;
        let payload = values;
        if (logical.channelArg) {
          channel = values[0];
          payload = values.slice(1);
        }
        const concrete = logical.commandByChannel
          ? logical.commandByChannel.get(channel ?? 1)
          : command;
        if (!concrete) {
          throw new Error(`command '${logical.id}' has no channel ${channel}`);
        }
        await instruments.assertCommand(entry.sessionId, concrete.id);
        const shouldSet = concrete.writable && (payload.length > 0 || !concrete.readable);
        const result = { ok: true, command: concrete.id };
        if (channel !== null) result.channel = channel;
        if (shouldSet) {
          const response = await sessions.request(entry.sessionId, 'set', {
            path: concrete.id,
            args: payload,
          });
          return { ...result, ...response };
        }
        if (!concrete.readable) throw new Error(`command '${concrete.id}' is not readable`);
        const response = await sessions.request(entry.sessionId, 'get', {
          path: concrete.id,
          args: payload,
        });
        return { ...result, ...response };
      },
    };
  }

  argumentSchema(arg) {
    const description = arg.unit ? `${arg.name} (${arg.unit})` : arg.name;
    const type = arg.type ?? 'any';
    if (type === 'bool') return { type: 'boolean', description };
    if (type === 'int') {
      const schema = { type: 'integer', description };
      if (arg.min !== undefined) schema.minimum = arg.min;
      if (arg.max !== undefined) schema.maximum = arg.max;
      return schema;
    }
    if (type === 'float') {
      const schema = { type: 'number', description };
      if (arg.min !== undefined) schema.minimum = arg.min;
      if (arg.max !== undefined) schema.maximum = arg.max;
      return schema;
    }
    if (type === 'enum') {
      const values = [...new Set([...(arg.values ?? []), ...(arg.special ?? [])])];
      return { type: 'string', enum: values, description };
    }
    if (type === 'string') return { type: 'string', description };
    return {
      description,
      oneOf: [{ type: 'string' }, { type: 'number' }, { type: 'boolean' }],
    };
  }

  async ensureOptions(sessionId, knownProfile) {
    if (this.options.has(sessionId)) return this.options.get(sessionId);
    const session = this.sessions.list().find((item) => item.session_id === sessionId);
    if (!session) throw new Error(`SCPI session '${sessionId}' is not open`);
    const profile = knownProfile ?? (await this.store.get(session.profile_id)).profile;
    const spec = profile.options ?? {};
    const result = {
      profile,
      installed: [],
      known: true,
      errors: [],
    };
    const probes = Array.isArray(spec.probes) ? spec.probes : [];
    for (const probe of probes) {
      try {
        const response = await this.sessions.request(
          sessionId,
          'get',
          { path: probe.command, args: probe.args ?? [] },
          { timeoutMs: 1500, closeOnTimeout: false },
        );
        const value = response.value;
        if (probe.type === 'list') {
          if (typeof value === 'string') {
            for (const item of value.split(',')) {
              const name = item.trim().replace(/^"|"$/g, '');
              if (name) result.installed.push(name);
            }
          }
        } else if (value === true || value === 1 || value === '1' || value === 'ON') {
          result.installed.push(probe.id);
        }
      } catch (error) {
        result.known = false;
        result.errors.push({ option: probe.id, message: error.message });
        break;
      }
    }
    for (const [option, implied] of Object.entries(spec.implies ?? {})) {
      if (result.installed.includes(option)) {
        for (const name of implied) {
          if (!result.installed.includes(name)) result.installed.push(name);
        }
      }
    }
    this.options.set(sessionId, result);
    return result;
  }

  async assertCommand(sessionId, path) {
    const options = await this.ensureOptions(sessionId);
    const command = options.profile.commands.find((item) => item.id === path);
    if (!command) throw new Error(`unknown command '${path}'`);
    const requires = Array.isArray(command.requires) ? command.requires : [];
    if (!requires.length) return;
    if (!options.known) {
      throw new Error(
        `option status is unknown; refusing '${path}' (requires ${requires.join(', ')})`,
      );
    }
    const missing = requires.filter((option) => !options.installed.includes(option));
    if (missing.length) {
      throw new Error(`command '${path}' requires option(s): ${missing.join(', ')}`);
    }
  }

  uniqueName(base) {
    let candidate = base;
    let index = 2;
    while (this.ctx.tools.get(candidate)) {
      candidate = `${base.slice(0, 60)}_${index++}`;
    }
    return candidate;
  }

  async unload({ tool, sessionId, session_id, prefix }) {
    const requestedSession = sessionId ?? session_id;
    let entry = null;
    if (typeof requestedSession === 'string' && requestedSession) {
      entry = this.loaded.get(requestedSession) ?? null;
    } else if (typeof prefix === 'string' && prefix) {
      entry = [...this.loaded.values()].find((item) => item.toolPrefix === prefix) ?? null;
    } else if (typeof tool === 'string' && tool) {
      entry = [...this.loaded.values()].find(
        (item) =>
          item.instrumentTool?.name === tool ||
          item.tools.some((candidate) => candidate.name === tool),
      ) ?? null;
    }
    if (!entry) throw new Error('loaded instrument tools not found');
    const id = entry.sessionId;
    const count = entry.tools.length;
    await this.sessions.close(id).catch(() => {});
    this.disposeEntry(entry);
    this.loaded.delete(id);
    return {
      unloaded: true,
      tools_unloaded: count,
      session_id: id,
      instrument_tool: entry.instrumentTool?.name ?? null,
      tool_prefix: entry.toolPrefix,
    };
  }
}

/**
 * 构建逻辑工具集与通道折叠：
 * 1. 过滤未安装硬件选件的命令（根据前面 probeOptions 的实测结果）；
 * 2. 识别 ch1.*, ch2.* 等按物理通道命名的命令；
 * 3. 检查通道对称性：若参数类型、读写属性完全一致，合并为带 `channel` 参数的单一逻辑工具；
 * 4. 若通道间属性不一致（例如仅通道 1 具备特殊耦合），则优雅降级为独立工具。
 */
function buildLogicalTools(profile, options) {
  const capabilityDefs = Array.isArray(profile.capabilities) ? profile.capabilities : [];
  const channels = Number.isInteger(profile.channels)
    ? Math.max(0, profile.channels)
    : deriveChannels(profile);
  const groups = new Map();
  const singles = [];
  const seen = new Set();
  let gatedCommands = 0;
  for (const command of profile.commands) {
    if (typeof command.id !== 'string' || !command.id) continue;
    if (seen.has(command.id)) continue;
    seen.add(command.id);
    const requires = Array.isArray(command.requires) ? command.requires : [];
    const available =
      requires.length === 0 ||
      (options.known && requires.every((option) => options.installed.includes(option)));
    if (!available) {
      gatedCommands += 1;
      continue;
    }
    const capability = capabilityDefs.length
      ? capabilityForCommand(profile, command.id)
      : 'commands';
    const match = command.id.match(/^ch([0-9]+)\.(.+)$/);
    if (match) {
      const channel = Number(match[1]);
      if (channels === 0 || channel < 1 || channel > channels) continue;
      const suffix = match[2];
      const key = `${capability}\u0000${suffix}`;
      if (!groups.has(key)) groups.set(key, { capability, suffix, members: [] });
      groups.get(key).members.push({ command, channel });
    } else {
      singles.push({
        id: command.id,
        capability,
        command,
        args: Array.isArray(command.args) ? command.args : [],
        readable: command.readable,
        writable: command.writable,
        channelArg: null,
        commandByChannel: null,
      });
    }
  }

  const tools = [...singles];
  const used = new Set(tools.map((tool) => tool.id));
  for (const group of groups.values()) {
    group.members.sort((left, right) => left.channel - right.channel);
    const first = group.members[0].command;
    const shape = JSON.stringify({
      args: first.args ?? [],
      writable: first.writable,
      readable: first.readable,
      requires: first.requires ?? [],
    });
    const asymmetric = group.members.some(
      (member) =>
        JSON.stringify({
          args: member.command.args ?? [],
          writable: member.command.writable,
          readable: member.command.readable,
          requires: member.command.requires ?? [],
        }) !== shape,
    );
    if (asymmetric) {
      for (const member of group.members) {
        tools.push({
          id: member.command.id,
          capability: group.capability,
          command: member.command,
          args: Array.isArray(member.command.args) ? member.command.args : [],
          readable: member.command.readable,
          writable: member.command.writable,
          channelArg: null,
          commandByChannel: null,
        });
      }
      continue;
    }
    const channelArg = channels > 1
      ? {
          name: 'channel',
          type: 'int',
          min: 1,
          max: channels,
          description: 'Analog channel number',
        }
      : null;
    let id = `${group.capability}_${group.suffix.replace(/\./g, '_')}`;
    let suffix = 2;
    while (used.has(id)) id = `${group.capability}_${group.suffix.replace(/\./g, '_')}_${suffix++}`;
    used.add(id);
    const commandByChannel = new Map();
    for (const member of group.members) commandByChannel.set(member.channel, member.command);
    tools.push({
      id,
      capability: group.capability,
      command: first,
      args: channelArg ? [channelArg, ...(first.args ?? [])] : [...(first.args ?? [])],
      readable: first.readable,
      writable: first.writable,
      channelArg,
      commandByChannel,
    });
  }

  const availableByCapability = new Map();
  for (const tool of tools) {
    availableByCapability.set(
      tool.capability,
      (availableByCapability.get(tool.capability) ?? 0) + 1,
    );
  }
  return { tools, channels, gatedCommands, availableByCapability };
}

function availableCounts(tools) {
  const counts = new Map();
  for (const tool of tools) {
    counts.set(tool.capability, (counts.get(tool.capability) ?? 0) + 1);
  }
  return counts;
}

function deriveChannels(profile) {
  let max = 0;
  for (const command of profile.commands ?? []) {
    const match = String(command.id ?? '').match(/^ch([0-9]+)\./);
    if (match) max = Math.max(max, Number(match[1]));
  }
  return max;
}

function validateInput(command, input) {
  const object = input && typeof input === 'object' && !Array.isArray(input) ? input : {};
  const args = Array.isArray(command.args) ? command.args : [];
  const selectorArgs = new Set(command.selectorArgs ?? []);
  const known = new Set(args.map((arg) => arg.name));
  for (const key of Object.keys(object)) {
    if (!known.has(key)) throw new Error(`command '${command.id}' has no argument '${key}'`);
  }
  if (!args.length) return [];
  const present = args.filter((arg) => object[arg.name] !== undefined);
  if (!present.length) {
    if (command.readable && command.writable && selectorArgs.size === 0) return [];
    throw new Error(`command '${command.id}' is missing arguments: ${args.map((arg) => arg.name).join(', ')}`);
  }
  if (command.readable && command.writable) {
    const selectorsOnly = present.every((arg) => selectorArgs.has(arg.name));
    if (selectorsOnly) {
      return present.map((arg) => validateArg(command, arg, object[arg.name]));
    }
  }
  if (present.length !== args.length) {
    const missing = args.filter((arg) => object[arg.name] === undefined).map((arg) => arg.name);
    throw new Error(`command '${command.id}' is missing argument(s): ${missing.join(', ')}`);
  }
  return args.map((arg) => validateArg(command, arg, object[arg.name]));
}

function validateArg(command, arg, value) {
  if (value === undefined) {
    throw new Error(`command '${command.id}' is missing argument '${arg.name}'`);
  }
  if (typeof value === 'string' && (arg.special ?? []).some((item) => item.toUpperCase() === value.toUpperCase())) {
    return value;
  }
  const type = arg.type ?? 'any';
  if (type === 'bool') {
    if (typeof value !== 'boolean') throw new Error(`'${arg.name}' must be a boolean`);
    return value;
  }
  if (type === 'int') {
    if (typeof value !== 'number' || !Number.isInteger(value)) {
      throw new Error(`'${arg.name}' must be an integer`);
    }
    checkRange(command, arg, value);
    return value;
  }
  if (type === 'float') {
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      throw new Error(`'${arg.name}' must be a finite number`);
    }
    checkRange(command, arg, value);
    return value;
  }
  if (type === 'enum') {
    if (typeof value !== 'string') throw new Error(`'${arg.name}' must be a string enum value`);
    const allowed = [...(arg.values ?? []), ...(arg.special ?? [])];
    if (!allowed.some((item) => item.toUpperCase() === value.toUpperCase())) {
      throw new Error(`'${arg.name}' must be one of ${allowed.join(', ')}`);
    }
    return value;
  }
  if (type === 'string') {
    if (typeof value !== 'string') throw new Error(`'${arg.name}' must be a string`);
    return value;
  }
  if (!['string', 'number', 'boolean'].includes(typeof value)) {
    throw new Error(`'${arg.name}' must be a string, number, or boolean`);
  }
  return value;
}

function checkRange(command, arg, value) {
  if (arg.min !== undefined && value < arg.min) {
    throw new Error(`command '${command.id}' argument '${arg.name}' is below minimum ${arg.min}`);
  }
  if (arg.max !== undefined && value > arg.max) {
    throw new Error(`command '${command.id}' argument '${arg.name}' is above maximum ${arg.max}`);
  }
}

function resolveCapabilities(profile, raw) {
  const defs = Array.isArray(profile.capabilities) ? profile.capabilities : [];
  if (!defs.length) return [];
  const all = defs.map((cap) => cap.id);
  let requested;
  if (raw === undefined || raw === null) {
    requested = [];
  } else if (raw === '*') {
    requested = all;
  } else if (Array.isArray(raw)) {
    requested = raw;
  } else if (typeof raw === 'string') {
    requested = [raw];
  } else {
    throw new Error('capabilities must be an array of strings or "*"');
  }
  const ids = [...new Set(requested.map((value) => String(value).trim()).filter(Boolean))];
  if (ids.includes('*')) return all;
  for (const id of ids) {
    if (!all.includes(id)) {
      throw new Error(`unknown capability '${id}'; available: ${all.join(', ')}`);
    }
  }
  return ids;
}

function capabilityForCommand(profile, commandId) {
  const defs = Array.isArray(profile.capabilities) ? profile.capabilities : [];
  for (const cap of defs) {
    const patterns = Array.isArray(cap.match) ? cap.match : [];
    if (patterns.some((pattern) => globMatch(pattern, commandId))) return cap.id;
  }
  return 'other';
}

function capabilitySummaries(profile, availableByCapability, selected) {
  const defs = Array.isArray(profile.capabilities) ? profile.capabilities : [];
  return defs.map((cap) => ({
    id: cap.id,
    name: cap.name,
    description: cap.description,
    tool_count: availableByCapability.get(cap.id) ?? 0,
    selected: selected.includes(cap.id),
  }));
}

function globMatch(pattern, value) {
  const escaped = String(pattern)
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '.*')
    .replace(/\?/g, '.');
  return new RegExp(`^${escaped}$`).test(value);
}

function serialFromIdn(idn) {
  const parts = String(idn ?? '').split(',');
  return parts[2] && parts[2].trim();
}

function modelFromIdn(idn, fallback) {
  const parts = String(idn ?? '').split(',');
  return (parts[1] && parts[1].trim()) || fallback || 'instrument';
}

function slug(value) {
  return (
    String(value)
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '_')
      .replace(/^_+|_+$/g, '')
      .slice(0, 28) || 'instrument'
  );
}
