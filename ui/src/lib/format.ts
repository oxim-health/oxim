// Display formatting for times, durations, sizes and numbers.

const pad = (value: number, length = 2) => String(value).padStart(length, '0');

/** A timestamp as local `YYYY-MM-DD HH:MM:SS`, or an em dash. */
export function formatTime(value: string | null | undefined): string {
  if (!value) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}

/** A time of day, `HH:MM:SS`, from milliseconds since the epoch. */
export function formatClock(millis: number): string {
  const date = new Date(millis);
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** A duration in seconds as `3d 4h`, `2h 5m`, `4m 10s` or `12s`. */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const days = Math.floor(s / 86_400);
  const hours = Math.floor((s % 86_400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const rest = s % 60;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${rest}s`;
  return `${rest}s`;
}

/** How long ago a timestamp was, relative to `now`. */
export function formatAge(value: string | null | undefined, now: Date = new Date()): string {
  if (!value) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return `${formatDuration((now.getTime() - date.getTime()) / 1000)} ago`;
}

/** A byte count as `512 B`, `4.2 KB` or `1.3 MB`. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const numbers = new Intl.NumberFormat('en');

export function formatCount(value: number): string {
  return numbers.format(value);
}

/** A `datetime-local` input value as an RFC 3339 UTC timestamp. */
export function localInputToIso(value: string): string | undefined {
  if (!value) return undefined;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString();
}

/** An RFC 3339 timestamp as a `datetime-local` input value. */
export function isoToLocalInput(value: string | null | undefined): string {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return '';
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    `T${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/** Human-readable names of permissions. */
export function permissionLabel(permission: string): string {
  const text = permission.replace(/_/g, ' ');
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** Title case for status words: `retrying` becomes `Retrying`. */
export function statusLabel(status: string): string {
  return status.charAt(0).toUpperCase() + status.slice(1).replace(/_/g, ' ');
}
