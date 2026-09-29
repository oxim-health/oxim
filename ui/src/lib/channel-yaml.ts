// A read-only structured view of a channel file, parsed in the browser so
// the editor can show it while typing. The server remains the authority:
// it validates the YAML against the component registry when saving.

import { LineCounter, parseDocument } from 'yaml';

export interface Step {
  type: string;
  /** The remaining settings as `key: value` text. */
  settings: [string, string][];
}

export interface DestinationView {
  id: string;
  type: string;
  filters: Step[];
  transformers: Step[];
  encoder: Step | null;
  queue: [string, string][];
  settings: [string, string][];
}

export interface ChannelView {
  id: string;
  name: string | null;
  description: string | null;
  enabled: boolean;
  source: {
    id: string;
    type: string;
    dataType: string;
    normalize: boolean;
    response: [string, string][];
    settings: [string, string][];
  };
  filters: Step[];
  transformers: Step[];
  destinations: DestinationView[];
}

export interface YamlProblem {
  message: string;
  line: number | null;
  column: number | null;
}

type Plain = Record<string, unknown>;

function isObject(value: unknown): value is Plain {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function text(value: unknown): string {
  if (value === null || value === undefined) return '';
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return JSON.stringify(value);
}

function entries(value: unknown, skip: string[] = []): [string, string][] {
  if (!isObject(value)) return [];
  return Object.entries(value)
    .filter(([key]) => !skip.includes(key))
    .map(([key, item]) => [key, text(item)]);
}

function step(value: unknown): Step {
  const object = isObject(value) ? value : {};
  return { type: text(object.type) || '(no type)', settings: entries(object, ['type']) };
}

function steps(value: unknown): Step[] {
  return Array.isArray(value) ? value.map(step) : [];
}

/** Extracts `line N` / `column M` from a server or parser message. */
export function locate(message: string): { line: number | null; column: number | null } {
  const line = /line\s+(\d+)/i.exec(message);
  const column = /column\s+(\d+)/i.exec(message);
  return {
    line: line ? Number(line[1]) : null,
    column: column ? Number(column[1]) : null,
  };
}

/** Parses channel YAML into the structured view, or reports why it
 * cannot be read. */
export function readChannel(yaml: string): { view: ChannelView | null; problems: YamlProblem[] } {
  const lines = new LineCounter();
  const document = parseDocument(yaml, { prettyErrors: false, lineCounter: lines });
  if (document.errors.length > 0) {
    return {
      view: null,
      problems: document.errors.map((error) => {
        const position = lines.linePos(error.pos[0]);
        return {
          message: error.message.split('\n')[0] ?? error.message,
          line: position.line,
          column: position.col,
        };
      }),
    };
  }
  const data: unknown = document.toJS();
  if (!isObject(data)) {
    return { view: null, problems: [{ message: 'A channel file is a YAML mapping.', line: 1, column: 1 }] };
  }
  const source = isObject(data.source) ? data.source : {};
  const view: ChannelView = {
    id: text(data.id),
    name: data.name === undefined ? null : text(data.name),
    description: data.description === undefined ? null : text(data.description),
    enabled: data.enabled !== false,
    source: {
      id: text(source.id) || 'source',
      type: text(source.type),
      dataType: text(source.data_type),
      normalize: source.normalize === true,
      response: entries(source.response),
      settings: entries(source.settings),
    },
    filters: steps(data.filters),
    transformers: steps(data.transformers),
    destinations: (Array.isArray(data.destinations) ? data.destinations : []).map((item) => {
      const destination = isObject(item) ? item : {};
      return {
        id: text(destination.id),
        type: text(destination.type),
        filters: steps(destination.filters),
        transformers: steps(destination.transformers),
        encoder: destination.encoder === undefined ? null : step(destination.encoder),
        queue: entries(destination.queue),
        settings: entries(destination.settings),
      };
    }),
  };
  const problems: YamlProblem[] = [];
  if (!view.id) problems.push({ message: 'The channel has no id.', line: null, column: null });
  if (!view.source.type) problems.push({ message: 'The source has no type.', line: null, column: null });
  if (!view.source.dataType) {
    problems.push({ message: 'The source has no data_type.', line: null, column: null });
  }
  return { view, problems };
}

/** A starting point for a new channel. */
export function channelTemplate(id: string): string {
  return `id: ${id || 'new-channel'}
name: New channel
enabled: true
source:
  type: mllp
  data_type: hl7v2
  settings:
    listen: 127.0.0.1:2575
destinations:
  - id: archive
    type: file
    settings:
      directory: archive
`;
}
