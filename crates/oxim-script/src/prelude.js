// The script API, installed once in every QuickJS context before the
// channel's script is compiled. `native` holds the Rust bindings; `mirth`
// enables the Mirth Connect compatibility layer. Everything a script can
// reach is defined here: there is no filesystem, network, timer or module
// access, and code generation from strings is disabled at the end.
(function (native, mirth) {
  'use strict';
  const g = globalThis;
  const define = (name, value) =>
    Object.defineProperty(g, name, {
      value,
      writable: false,
      enumerable: false,
      configurable: false,
    });
  const text = (value) => (value === undefined || value === null ? '' : String(value));
  const describe = (value) =>
    value === null ? 'null' : Array.isArray(value) ? 'an array' : typeof value;
  const bytes = (value) => {
    if (value instanceof ArrayBuffer) {
      return Array.from(new Uint8Array(value));
    }
    if (ArrayBuffer.isView(value)) {
      return Array.from(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
    }
    return null;
  };

  // msg: the parsed message, addressed with the paths of its data type.
  const api = Object.freeze({
    get(path) {
      const value = native.get(text(path));
      return value === undefined ? null : value;
    },
    set(path, value) {
      native.set(text(path), text(value));
    },
    get raw() {
      return native.raw();
    },
    get dataType() {
      return native.dataType();
    },
  });

  const logAt = (level) => (...parts) => native.log(level, parts.map(text).join(' '));
  const log = Object.freeze({
    debug: logAt('debug'),
    info: logAt('info'),
    warn: logAt('warn'),
    error: logAt('error'),
  });
  define('log', log);

  define('reply', function reply(data, dataType) {
    const type = dataType === undefined || dataType === null ? null : String(dataType);
    const binary = bytes(data);
    if (binary === null) {
      native.replyText(text(data), type);
    } else {
      native.replyBytes(binary, type);
    }
  });

  define('__oxim_bytes', function (value) {
    const binary = bytes(value);
    if (binary === null) {
      throw new TypeError(
        'an encoder script must return a string or a Uint8Array, not ' + describe(value),
      );
    }
    return binary;
  });

  define('__oxim_install', function (main) {
    define('__oxim_main', main);
  });

  define('__oxim_begin', function (input) {
    const state = JSON.parse(input);
    g.clinical = state.clinical;
    g.vars = state.vars;
  });

  define('__oxim_end', function () {
    const vars = {};
    const source = g.vars;
    if (source !== null && typeof source === 'object') {
      for (const key of Object.keys(source)) {
        const value = source[key];
        if (value === undefined || value === null) {
          continue;
        }
        vars[key] =
          typeof value === 'string'
            ? value
            : typeof value === 'object'
              ? JSON.stringify(value)
              : String(value);
      }
    }
    const clinical = g.clinical === undefined ? null : g.clinical;
    return [JSON.stringify(clinical), JSON.stringify(vars)];
  });

  if (!mirth) {
    define('msg', api);
  } else {
    // Mirth Connect compatibility: E4X-style access to HL7 v2 messages
    // (msg['PID']['PID.5']['PID.5.1']), the variable maps and logger.
    const SEGMENT = /^[A-Z][A-Z0-9]{2}$/;
    const INDEX = /^(0|[1-9][0-9]*)$/;
    const NODE = Symbol('oxim.node');

    const pathOf = (node) => {
      let path = node.segment + (node.occurrence ? '[' + node.occurrence + ']' : '');
      if (node.field === undefined) {
        return path;
      }
      path += '-' + node.field + (node.repetition ? '[' + node.repetition + ']' : '');
      if (node.component !== undefined) {
        path += '.' + node.component;
      }
      if (node.subcomponent !== undefined) {
        path += '.' + node.subcomponent;
      }
      return path;
    };
    const valueOf = (node) => {
      const value =
        node.field === undefined
          ? native.segment(node.segment, node.occurrence || 1)
          : native.get(pathOf(node));
      return value === undefined ? '' : value;
    };
    const lengthOf = (node) => {
      if (node.field === undefined && node.occurrence === undefined) {
        return native.count(node.segment);
      }
      if (node.field !== undefined && node.repetition === undefined && node.component === undefined) {
        return native.count(pathOf(node));
      }
      return valueOf(node) === '' ? 0 : 1;
    };
    const depthOf = (node) =>
      node.field === undefined
        ? 0
        : node.component === undefined
          ? 1
          : node.subcomponent === undefined
            ? 2
            : 3;
    // The node that `key` selects below `node`, or null.
    const child = (node, key) => {
      if (INDEX.test(key)) {
        const index = Number(key) + 1;
        if (node.field === undefined && node.occurrence === undefined) {
          return Object.assign({}, node, { occurrence: index });
        }
        if (node.field !== undefined && node.repetition === undefined && node.component === undefined) {
          return Object.assign({}, node, { repetition: index });
        }
        return index === 1 ? node : null;
      }
      const parts = key.split('.');
      if (parts[0] !== node.segment) {
        return null;
      }
      const numbers = parts.slice(1).map(Number);
      const depth = depthOf(node);
      if (numbers.length !== depth + 1 || numbers.some((n) => !Number.isInteger(n) || n < 1)) {
        return null;
      }
      if (depth >= 1 && numbers[0] !== node.field) {
        return null;
      }
      if (depth >= 2 && numbers[1] !== node.component) {
        return null;
      }
      const next = Object.assign({}, node);
      if (depth === 0) {
        next.field = numbers[0];
      } else if (depth === 1) {
        next.component = numbers[1];
      } else if (depth === 2) {
        next.subcomponent = numbers[2];
      } else {
        return null;
      }
      return next;
    };
    const wrap = (node) =>
      new Proxy(
        {},
        {
          get(target, key) {
            if (key === NODE) {
              return true;
            }
            if (key === Symbol.toPrimitive) {
              return () => valueOf(node);
            }
            if (typeof key === 'symbol') {
              return undefined;
            }
            if (key === 'toString' || key === 'text' || key === 'valueOf' || key === 'toXMLString' || key === 'toJSON') {
              return () => valueOf(node);
            }
            if (key === 'length') {
              return () => lengthOf(node);
            }
            const next = child(node, key);
            return next === null ? undefined : wrap(next);
          },
          set(target, key, value) {
            const next = typeof key === 'string' ? child(node, key) : null;
            if (next === null || next.field === undefined) {
              throw new TypeError('cannot assign ' + String(key) + ' below ' + pathOf(node));
            }
            native.set(pathOf(next), text(value));
            return true;
          },
        },
      );
    const isNode = (value) => value !== null && typeof value === 'object' && value[NODE] === true;

    const message = new Proxy(
      {},
      {
        get(target, key) {
          if (key === 'get' || key === 'set' || key === 'raw' || key === 'dataType') {
            return api[key];
          }
          if (key === Symbol.toPrimitive || key === 'toString') {
            return () => api.raw;
          }
          if (typeof key === 'string' && SEGMENT.test(key)) {
            return wrap({ segment: key });
          }
          return undefined;
        },
        set(target, key) {
          throw new TypeError(
            'msg[' + String(key) + '] cannot be replaced; assign fields such as ' +
              "msg['PID']['PID.5']['PID.5.1']",
          );
        },
      },
    );
    define('msg', message);
    define('tmp', message);

    // Per-message maps live in the message variables: channelMap entries
    // under their own names, the others with a prefix.
    const store = (value) =>
      value === undefined || value === null
        ? ''
        : typeof value === 'string'
          ? value
          : isNode(value) || typeof value !== 'object'
            ? String(value)
            : JSON.stringify(value);
    const CONNECTOR = 'connectorMap.';
    const RESPONSE = 'responseMap.';
    const variableMap = (prefix) =>
      Object.freeze({
        put(key, value) {
          const old = this.get(key);
          g.vars[prefix + text(key)] = store(value);
          return old;
        },
        get(key) {
          const value = g.vars[prefix + text(key)];
          return value === undefined ? null : value;
        },
        containsKey(key) {
          return Object.prototype.hasOwnProperty.call(g.vars, prefix + text(key));
        },
        remove(key) {
          const old = this.get(key);
          delete g.vars[prefix + text(key)];
          return old;
        },
        keySet() {
          return Object.keys(g.vars)
            .filter((key) =>
              prefix === ''
                ? !key.startsWith(CONNECTOR) && !key.startsWith(RESPONSE)
                : key.startsWith(prefix),
            )
            .map((key) => key.slice(prefix.length));
        },
      });
    // Deployment-wide maps are kept by OXIM as JSON text.
    const sharedMap = (scope) =>
      Object.freeze({
        put(key, value) {
          const old = this.get(key);
          const stored = isNode(value) ? String(value) : value === undefined ? null : value;
          native.globalPut(scope, text(key), JSON.stringify(stored) ?? 'null');
          return old;
        },
        get(key) {
          const value = native.globalGet(scope, text(key));
          return value === undefined ? null : JSON.parse(value);
        },
        containsKey(key) {
          return native.globalGet(scope, text(key)) !== undefined;
        },
        remove(key) {
          const old = this.get(key);
          native.globalRemove(scope, text(key));
          return old;
        },
        keySet() {
          return native.globalKeys(scope);
        },
      });
    const channelMap = variableMap('');
    const connectorMap = variableMap(CONNECTOR);
    const responseMap = variableMap(RESPONSE);
    const globalChannelMap = sharedMap('channel');
    const globalMap = sharedMap('global');
    define('channelMap', channelMap);
    define('connectorMap', connectorMap);
    define('responseMap', responseMap);
    define('globalChannelMap', globalChannelMap);
    define('globalMap', globalMap);
    const accessor = (map) => (key, value) => (value === undefined ? map.get(key) : map.put(key, value));
    define('$c', accessor(channelMap));
    define('$co', accessor(connectorMap));
    define('$r', accessor(responseMap));
    define('$gc', accessor(globalChannelMap));
    define('$g', accessor(globalMap));
    define('$', function (key) {
      for (const map of [responseMap, connectorMap, channelMap, globalChannelMap, globalMap]) {
        if (map.containsKey(key)) {
          return map.get(key);
        }
      }
      return null;
    });
    define(
      'logger',
      Object.freeze({
        trace: logAt('debug'),
        debug: logAt('debug'),
        info: logAt('info'),
        warn: logAt('warn'),
        error: logAt('error'),
      }),
    );
  }

  // No code generation from strings: eval and the Function constructors
  // throw. The channel's script itself is compiled by OXIM.
  const blocked = new Proxy(Function, {
    apply() {
      throw new EvalError('code generation from strings is disabled');
    },
    construct() {
      throw new EvalError('code generation from strings is disabled');
    },
  });
  for (const f of [function () {}, function* () {}, async function () {}, async function* () {}]) {
    Object.defineProperty(Object.getPrototypeOf(f), 'constructor', {
      value: blocked,
      writable: false,
      enumerable: false,
      configurable: false,
    });
  }
  define('Function', blocked);
  delete g.eval;
});
