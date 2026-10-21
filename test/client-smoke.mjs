import fs from 'node:fs';
import vm from 'node:vm';

const source = fs.readFileSync(new URL('../client.js', import.meta.url), 'utf8');
let factory;
const context = {
  window: { __ModuleLoader__: { load(spec) { factory = spec.factory; } } },
  console,
};
vm.createContext(context);
vm.runInContext(source, context);
if (!factory) throw new Error('client module did not call __ModuleLoader__.load');

let Component;
const React = {
  createElement(type, props, ...children) {
    return { type, props, children };
  },
  useState(value) {
    return [value, () => {}];
  },
  useCallback(fn) {
    return fn;
  },
  useEffect() {},
  useRef(value) {
    return { current: value };
  },
};

const state = {
  profileDir: '/tmp/profiles',
  settings: { verifyIdn: true },
  binaryPath: '/tmp/bin/scpi',
  binaryError: null,
  profiles: [{ id: 'rigol.dm3058', name: 'DM3058', commands: 126, valid: true }],
  bundled: [{ bundled_name: 'dm3058', name: 'DM3058', commands: 126, valid: true }],
  sessions: [],
};

// The component calls useState in a fixed order: state, error, notice, busy,
// dir, verifyIdn, selected, found.
const initial = [state, '', '', false, '', true, null, null];
let hook = 0;
const hookReact = {
  ...React,
  useState() {
    const value = initial[hook++];
    return [value, () => {}];
  },
};

const plugin = factory((name) => {
  if (name === 'react') return hookReact;
  throw new Error(`unexpected module ${name}`);
});
plugin.apply({
  slots: {
    inject(_name, callback) {
      callback();
    },
    register(_meta, component) {
      Component = component;
    },
  },
});
if (!Component) throw new Error('settings.section was not registered');

const tree = Component();
if (tree?.type !== 'div') throw new Error('settings component did not render');
console.log('client smoke ok');
