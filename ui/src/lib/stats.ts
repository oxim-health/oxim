// Dashboard figures from the server's `stats` events.

import type { StatsEvent } from './api';

export interface ChannelFigures {
  channel: string;
  deployed: boolean;
  /** Every stored message. */
  received: number;
  filtered: number;
  /** Deliveries waiting: queued, being sent or waiting for a retry. */
  queued: number;
  sent: number;
  /** Messages that failed processing plus deliveries that gave up. */
  errored: number;
  /** Deliveries waiting for a retry (a subset of `queued`). */
  retrying: number;
}

function blank(channel: string, deployed: boolean): ChannelFigures {
  return { channel, deployed, received: 0, filtered: 0, queued: 0, sent: 0, errored: 0, retrying: 0 };
}

/** Per-channel figures, sorted by channel, including deployed channels
 * without messages. */
export function summarize(event: StatsEvent): ChannelFigures[] {
  const deployed = new Set(event.deployed);
  const rows = new Map<string, ChannelFigures>();
  const row = (channel: string) => {
    let figures = rows.get(channel);
    if (!figures) {
      figures = blank(channel, deployed.has(channel));
      rows.set(channel, figures);
    }
    return figures;
  };
  for (const channel of event.deployed) row(channel);
  for (const { channel, status, count } of event.messages) {
    const figures = row(channel);
    figures.received += count;
    if (status === 'filtered') figures.filtered += count;
    if (status === 'error') figures.errored += count;
  }
  for (const { channel, status, count } of event.deliveries) {
    const figures = row(channel);
    switch (status) {
      case 'queued':
      case 'sending':
        figures.queued += count;
        break;
      case 'retrying':
        figures.queued += count;
        figures.retrying += count;
        break;
      case 'sent':
        figures.sent += count;
        break;
      case 'failed':
        figures.errored += count;
        break;
      case 'filtered':
        break;
    }
  }
  return [...rows.values()].sort((a, b) => a.channel.localeCompare(b.channel));
}

/** Totals over every channel. */
export function totals(rows: ChannelFigures[]): Omit<ChannelFigures, 'channel' | 'deployed'> {
  const sum = { received: 0, filtered: 0, queued: 0, sent: 0, errored: 0, retrying: 0 };
  for (const row of rows) {
    sum.received += row.received;
    sum.filtered += row.filtered;
    sum.queued += row.queued;
    sum.sent += row.sent;
    sum.errored += row.errored;
    sum.retrying += row.retrying;
  }
  return sum;
}
