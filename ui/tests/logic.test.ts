import { describe, expect, it } from 'vitest';
import { locate, readChannel } from '../src/lib/channel-yaml';
import { parseCsv, validateTable, writeCsv } from '../src/lib/csv';
import { formatBytes, formatDuration, isoToLocalInput, localInputToIso, statusLabel } from '../src/lib/format';
import { isClientPath, isCurrent, matchRoute } from '../src/lib/routes';
import { astmSegments, hl7Segments, segmentsFor } from '../src/lib/segments';
import { summarize, totals } from '../src/lib/stats';

describe('routes', () => {
  it('matches pages and parameters', () => {
    expect(matchRoute('/')).toEqual({ name: 'dashboard', params: {} });
    expect(matchRoute('/channels/new')).toEqual({ name: 'channel-new', params: {} });
    expect(matchRoute('/channels/lab-orders/')).toEqual({ name: 'channel', params: { id: 'lab-orders' } });
    expect(matchRoute('/tables/chemistry%20codes.csv')).toEqual({
      name: 'table',
      params: { name: 'chemistry codes.csv' },
    });
    expect(matchRoute('/messages/01J0/extra').name).toBe('not-found');
    expect(matchRoute('/tables/%E0%A4%A').name).toBe('not-found');
  });

  it('marks the current section and keeps API links out of the router', () => {
    expect(isCurrent('/', '/')).toBe(true);
    expect(isCurrent('/', '/messages')).toBe(false);
    expect(isCurrent('/messages', '/messages/01J0')).toBe(true);
    expect(isCurrent('/messages', '/messagesX')).toBe(false);
    expect(isClientPath('/channels')).toBe(true);
    expect(isClientPath('/api/v1/openapi.json')).toBe(false);
    expect(isClientPath('/metrics')).toBe(false);
    expect(isClientPath('//evil.example')).toBe(false);
    expect(isClientPath('https://example.org/')).toBe(false);
  });
});

describe('dashboard figures', () => {
  it('sums messages and deliveries per channel', () => {
    const rows = summarize({
      deployed: ['lab', 'idle'],
      messages: [
        { channel: 'lab', status: 'completed', count: 10 },
        { channel: 'lab', status: 'filtered', count: 2 },
        { channel: 'lab', status: 'error', count: 1 },
        { channel: 'old', status: 'completed', count: 4 },
      ],
      deliveries: [
        { channel: 'lab', destination: 'lis', status: 'sent', count: 9 },
        { channel: 'lab', destination: 'lis', status: 'queued', count: 3 },
        { channel: 'lab', destination: 'lis', status: 'retrying', count: 2 },
        { channel: 'lab', destination: 'lis', status: 'failed', count: 1 },
        { channel: 'lab', destination: 'archive', status: 'filtered', count: 5 },
      ],
    });
    expect(rows.map((row) => row.channel)).toEqual(['idle', 'lab', 'old']);
    expect(rows[0]).toMatchObject({ deployed: true, received: 0 });
    expect(rows[1]).toEqual({
      channel: 'lab',
      deployed: true,
      received: 13,
      filtered: 2,
      queued: 5,
      sent: 9,
      errored: 2,
      retrying: 2,
    });
    expect(rows[2]).toMatchObject({ deployed: false, received: 4 });
    expect(totals(rows)).toEqual({ received: 17, filtered: 2, queued: 5, sent: 9, errored: 2, retrying: 2 });
  });
});

describe('code table CSV', () => {
  it('parses quoted fields, CRLF and a byte order mark', () => {
    expect(parseCsv('﻿from,to,display\r\nGLU,2345-7,"Glucose, fasting"\r\nNA,2951-2,"Say ""hi"""\n')).toEqual([
      ['from', 'to', 'display'],
      ['GLU', '2345-7', 'Glucose, fasting'],
      ['NA', '2951-2', 'Say "hi"'],
    ]);
    expect(parseCsv('a,"multi\nline"')).toEqual([['a', 'multi\nline']]);
  });

  it('round-trips through writeCsv', () => {
    const rows = [
      ['from', 'to', 'display'],
      ['GLU', '2345-7', 'Glucose, fasting'],
      ['K', '2823-3', ' padded '],
    ];
    expect(parseCsv(writeCsv(rows))).toEqual(rows);
  });

  it('validates like the server', () => {
    expect(validateTable([['from', 'to'], ['GLU', '1']])).toEqual([]);
    expect(validateTable([['code', 'to']]).map((p) => p.message)).toEqual([
      'Unknown column "code". Use from, to, display, system and context.',
      'The "from" column is missing.',
    ]);
    const problems = validateTable([
      ['from', 'to', 'context'],
      ['GLU', '1', ''],
      ['GLU', '2', ''],
      ['GLU', '3', 'poct-1'],
      ['', '', ''],
      ['NA', '', ''],
    ]);
    expect(problems.map((p) => [p.row, p.column])).toEqual([
      [2, 'from'],
      [5, 'to'],
    ]);
  });
});

describe('message fields', () => {
  it('numbers HL7 fields like OXIM paths', () => {
    const segments = hl7Segments('MSH|^~\\&|LAB|HOSP\rPID|1||MRN1^^^HOSP^MR||Doe^Jane\rOBX|1|NM\rOBX|2|ST');
    expect(segments[0]!.fields.slice(0, 3)).toEqual([
      { path: 'MSH-1', value: '|' },
      { path: 'MSH-2', value: '^~\\&' },
      { path: 'MSH-3', value: 'LAB' },
    ]);
    expect(segments[1]!.fields[4]).toEqual({ path: 'PID-5', value: 'Doe^Jane' });
    expect(segments.map((segment) => [segment.id, segment.occurrence])).toEqual([
      ['MSH', 1],
      ['PID', 1],
      ['OBX', 1],
      ['OBX', 2],
    ]);
  });

  it('numbers ASTM fields from the record type', () => {
    const records = astmSegments('H|\\^&|||Analyzer\nR|1|^^^GLU|5.4|mmol/L');
    expect(records[1]!.fields.slice(0, 4)).toEqual([
      { path: 'R-1', value: 'R' },
      { path: 'R-2', value: '1' },
      { path: 'R-3', value: '^^^GLU' },
      { path: 'R-4', value: '5.4' },
    ]);
    expect(segmentsFor('json', '{}')).toBeNull();
  });
});

describe('channel YAML view', () => {
  it('reads the structure of a channel', () => {
    const { view, problems } = readChannel(`id: lab
name: Laboratory
source:
  type: mllp
  data_type: hl7v2
  normalize: true
  settings: {listen: 127.0.0.1:2575}
transformers:
  - {type: map-observations, table: chem.csv}
destinations:
  - id: lis
    type: mllp
    encoder: {type: hl7v2-oru-r01, sending_application: OXIM}
    queue: {ordering: strict}
    settings: {target: lis.example.org:2575}
`);
    expect(problems).toEqual([]);
    expect(view?.id).toBe('lab');
    expect(view?.source).toMatchObject({ type: 'mllp', dataType: 'hl7v2', normalize: true });
    expect(view?.source.settings).toEqual([['listen', '127.0.0.1:2575']]);
    expect(view?.transformers).toEqual([{ type: 'map-observations', settings: [['table', 'chem.csv']] }]);
    expect(view?.destinations[0]?.encoder?.type).toBe('hl7v2-oru-r01');
    expect(view?.destinations[0]?.queue).toEqual([['ordering', 'strict']]);
  });

  it('reports syntax errors with their position and missing parts', () => {
    const broken = readChannel('id: lab\nsource: [\n');
    expect(broken.view).toBeNull();
    expect(broken.problems[0]?.line).toBeGreaterThan(0);
    const incomplete = readChannel('name: x\n');
    expect(incomplete.problems.map((p) => p.message)).toContain('The channel has no id.');
  });

  it('finds positions in server messages', () => {
    expect(locate('invalid channel: unknown field `sourc` at line 3 column 1')).toEqual({ line: 3, column: 1 });
    expect(locate('unknown source type "mlp"')).toEqual({ line: null, column: null });
  });
});

describe('formatting', () => {
  it('formats durations, sizes and labels', () => {
    expect(formatDuration(12)).toBe('12s');
    expect(formatDuration(250)).toBe('4m 10s');
    expect(formatDuration(7500)).toBe('2h 5m');
    expect(formatDuration(3 * 86_400 + 4 * 3600)).toBe('3d 4h');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(4300)).toBe('4.2 KB');
    expect(statusLabel('retrying')).toBe('Retrying');
  });

  it('converts between datetime-local values and UTC timestamps', () => {
    const iso = localInputToIso('2026-09-29T14:30');
    expect(iso).toMatch(/^2026-09-29T\d\d:30:00\.000Z$/);
    expect(isoToLocalInput(iso)).toBe('2026-09-29T14:30');
    expect(localInputToIso('')).toBeUndefined();
  });
});
