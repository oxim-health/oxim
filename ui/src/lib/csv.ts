// CSV reading and writing for code tables (RFC 4180: comma separated,
// double quotes around fields with commas, quotes or line breaks), and the
// same validation rules the server applies before saving a table.

/** Parses CSV text into rows of fields. A UTF-8 byte order mark and a
 * final line break are ignored. */
export function parseCsv(text: string): string[][] {
  const input = text.startsWith('﻿') ? text.slice(1) : text;
  const rows: string[][] = [];
  let row: string[] = [];
  let field = '';
  let quoted = false;
  let index = 0;
  while (index < input.length) {
    const char = input[index] as string;
    if (quoted) {
      if (char === '"') {
        if (input[index + 1] === '"') {
          field += '"';
          index += 2;
          continue;
        }
        quoted = false;
      } else {
        field += char;
      }
      index += 1;
      continue;
    }
    if (char === '"' && field === '') {
      quoted = true;
    } else if (char === ',') {
      row.push(field);
      field = '';
    } else if (char === '\n' || char === '\r') {
      row.push(field);
      rows.push(row);
      row = [];
      field = '';
      if (char === '\r' && input[index + 1] === '\n') index += 1;
    } else {
      field += char;
    }
    index += 1;
  }
  if (field !== '' || row.length > 0) {
    row.push(field);
    rows.push(row);
  }
  return rows;
}

function quote(field: string): string {
  return /[",\r\n]/.test(field) || field !== field.trim() ? `"${field.replace(/"/g, '""')}"` : field;
}

/** Writes rows as CSV with `\n` line endings and a final line break. */
export function writeCsv(rows: string[][]): string {
  return rows.map((row) => row.map(quote).join(',')).join('\n') + '\n';
}

export const TABLE_COLUMNS = ['from', 'to', 'display', 'system', 'context'] as const;
export type TableColumn = (typeof TABLE_COLUMNS)[number];

export interface TableProblem {
  /** 1-based data row (0 for the header). */
  row: number;
  column?: string;
  message: string;
}

/** Checks a code table as the server does: known columns, `from` and `to`
 * present and filled, and no code defined twice for the same context. */
export function validateTable(rows: string[][]): TableProblem[] {
  const problems: TableProblem[] = [];
  const header = (rows[0] ?? []).map((name) => name.trim().toLowerCase());
  for (const name of header) {
    if (name !== '' && !(TABLE_COLUMNS as readonly string[]).includes(name)) {
      problems.push({
        row: 0,
        column: name,
        message: `Unknown column "${name}". Use from, to, display, system and context.`,
      });
    }
  }
  const from = header.indexOf('from');
  const to = header.indexOf('to');
  if (from < 0) problems.push({ row: 0, column: 'from', message: 'The "from" column is missing.' });
  if (to < 0) problems.push({ row: 0, column: 'to', message: 'The "to" column is missing.' });
  if (from < 0 || to < 0) return problems;
  const context = header.indexOf('context');
  const seen = new Map<string, number>();
  rows.slice(1).forEach((row, offset) => {
    const number = offset + 1;
    if (row.every((field) => field.trim() === '')) return;
    const code = (row[from] ?? '').trim();
    const target = (row[to] ?? '').trim();
    if (!code) problems.push({ row: number, column: 'from', message: `Row ${number}: "from" is empty.` });
    if (!target) problems.push({ row: number, column: 'to', message: `Row ${number}: "to" is empty.` });
    if (code) {
      const scope = context >= 0 ? (row[context] ?? '').trim() : '';
      const key = `${scope}\u0000${code}`;
      const first = seen.get(key);
      if (first !== undefined) {
        problems.push({
          row: number,
          column: 'from',
          message: `Row ${number}: code "${code}"${scope ? ` in context "${scope}"` : ''} is already defined in row ${first}.`,
        });
      } else {
        seen.set(key, number);
      }
    }
  });
  return problems;
}
