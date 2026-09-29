// A field-by-field view of HL7 v2 and ASTM messages for the message
// browser. The server keeps the lossless trees; this only splits text for
// display, numbering fields the way OXIM paths do (PID-5, R-4).

export interface Field {
  /** Position as used in paths: `PID-5`, `MSH-9`, `R-4`. */
  path: string;
  value: string;
}

export interface Segment {
  id: string;
  /** Occurrence of this segment id, starting at 1. */
  occurrence: number;
  fields: Field[];
}

function lines(text: string): string[] {
  return text.split(/\r\n|\r|\n/).filter((line) => line.length > 0);
}

/** Splits HL7 v2 (ER7). MSH-1 is the field separator and MSH-2 the
 * encoding characters, as in the standard. */
export function hl7Segments(text: string): Segment[] {
  const counts = new Map<string, number>();
  return lines(text).map((line) => {
    const separator = line.startsWith('MSH') && line.length > 3 ? (line[3] as string) : '|';
    const parts = line.split(separator);
    const id = parts[0] ?? '';
    const occurrence = (counts.get(id) ?? 0) + 1;
    counts.set(id, occurrence);
    const fields: Field[] = [];
    if (id === 'MSH' || id === 'FHS' || id === 'BHS') {
      fields.push({ path: `${id}-1`, value: separator });
      parts.slice(1).forEach((value, index) => fields.push({ path: `${id}-${index + 2}`, value }));
    } else {
      parts.slice(1).forEach((value, index) => fields.push({ path: `${id}-${index + 1}`, value }));
    }
    return { id, occurrence, fields };
  });
}

/** Splits ASTM E1394 records; the record type is field 1. */
export function astmSegments(text: string): Segment[] {
  const counts = new Map<string, number>();
  return lines(text).map((line) => {
    // Frame numbers and checksums are not part of stored messages, but a
    // leading STX-framed digit would be; strip control characters.
    const clean = line.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f]/g, '');
    const separator = clean.length > 1 ? (clean[1] as string) : '|';
    const parts = clean.split(separator);
    const id = parts[0] ?? '';
    const occurrence = (counts.get(id) ?? 0) + 1;
    counts.set(id, occurrence);
    const fields = parts.map((value, index) => ({ path: `${id}-${index + 1}`, value }));
    return { id, occurrence, fields };
  });
}

/** The structured view for a data type, or `null` when there is none. */
export function segmentsFor(dataType: string | null | undefined, text: string): Segment[] | null {
  if (dataType === 'hl7v2') return hl7Segments(text);
  if (dataType === 'astm') return astmSegments(text);
  return null;
}
