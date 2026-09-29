// Starts a real `oxim run` for the end-to-end tests: a temporary
// configuration with one MLLP channel, an administrator and a viewer, and
// one synthetic HL7 message already received.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer, Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export interface E2eState {
  baseURL: string;
  mllpPort: number;
  pid: number;
  dir: string;
  adminPassword: string;
  viewerPassword: string;
}

const here = dirname(fileURLToPath(import.meta.url));
export const UI_ROOT = resolve(here, '..');
export const REPO_ROOT = resolve(UI_ROOT, '..');
const STATE_FILE = join(here, '.state', 'state.json');

export const ADMIN_PASSWORD = 'e2e admin password';
export const VIEWER_PASSWORD = 'e2e viewer password';

/** A synthetic lab result; no real patient data. */
export const SAMPLE_HL7 = [
  'MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01|E2E0001|P|2.5.1',
  'PID|1||MRN0042^^^HOSP^MR||Testpatient^Synthetic||19700101|F',
  'OBR|1|ORD1||GLU^Glucose',
  'OBX|1|NM|GLU^Glucose||5.4|mmol/L|3.9-6.1|N|||F',
].join('\r');

export function readState(): E2eState {
  return JSON.parse(readFileSync(STATE_FILE, 'utf8')) as E2eState;
}

function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once('error', fail);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port = typeof address === 'object' && address ? address.port : 0;
      server.close(() => done(port));
    });
  });
}

function binary(): string {
  if (process.env.OXIM_BIN) return process.env.OXIM_BIN;
  const name = process.platform === 'win32' ? 'oxim.exe' : 'oxim';
  const path = join(REPO_ROOT, 'target', 'debug', name);
  if (!existsSync(path)) {
    const build = spawnSync('cargo', ['build', '-p', 'oxim'], { cwd: REPO_ROOT, stdio: 'inherit' });
    if (build.status !== 0) throw new Error('cargo build -p oxim failed');
  }
  return path;
}

function run(bin: string, args: string[], input?: string): void {
  const result = spawnSync(bin, args, { input, encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error(`oxim ${args.join(' ')} failed: ${result.stderr || result.stdout}`);
  }
}

async function waitFor(what: string, check: () => Promise<boolean>, timeoutMs = 30_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await check().catch(() => false)) return;
    await new Promise((done) => setTimeout(done, 200));
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** Sends one MLLP frame and resolves with the acknowledgment. */
export function sendMllp(port: number, message: string): Promise<string> {
  return new Promise((done, fail) => {
    const socket = new Socket();
    let received = '';
    socket.setTimeout(10_000, () => {
      socket.destroy();
      fail(new Error('no MLLP acknowledgment'));
    });
    socket.on('data', (chunk) => {
      received += chunk.toString('utf8');
      if (received.includes('\x1c')) {
        socket.end();
        done(received.replace(/[\x0b\x1c]/g, '').trim());
      }
    });
    socket.on('error', fail);
    socket.connect(port, '127.0.0.1', () => socket.write(`\x0b${message}\x1c\r`));
  });
}

export async function start(): Promise<E2eState> {
  const bin = binary();
  const dir = mkdtempSync(join(tmpdir(), 'oxim-e2e-'));
  for (const sub of ['channels', 'tables', 'data', 'archive']) mkdirSync(join(dir, sub));
  const httpPort = await freePort();
  const mllpPort = await freePort();
  const embedded = process.env.OXIM_E2E_EMBEDDED === '1';
  const uiDir = join(UI_ROOT, 'dist');
  if (!embedded && !existsSync(join(uiDir, 'index.html'))) {
    throw new Error('ui/dist is missing: run `npm run build` first');
  }
  const config = join(dir, 'oxim.yaml');
  writeFileSync(
    config,
    [
      'data_dir: data',
      'channels_dir: channels',
      'tables_dir: tables',
      'log:',
      '  level: warn',
      'reload:',
      '  enabled: true',
      '  interval: 1s',
      'server:',
      `  listen: 127.0.0.1:${httpPort}`,
      ...(embedded ? [] : [`  ui_dir: ${JSON.stringify(uiDir)}`]),
      '',
    ].join('\n'),
  );
  writeFileSync(
    join(dir, 'channels', 'lab.yaml'),
    [
      'id: lab',
      'name: Laboratory results',
      'source:',
      '  type: mllp',
      '  data_type: hl7v2',
      '  settings:',
      `    listen: 127.0.0.1:${mllpPort}`,
      'destinations:',
      '  - id: archive',
      '    type: file',
      '    settings:',
      `      directory: ${JSON.stringify(join(dir, 'archive'))}`,
      '',
    ].join('\n'),
  );
  writeFileSync(join(dir, 'tables', 'chemistry.csv'), 'from,to,display\nGLU,2345-7,Glucose\n');
  run(bin, ['users', 'create-admin', '--password-stdin', '-c', config], `${ADMIN_PASSWORD}\n`);
  run(bin, ['users', 'add', 'viewer', '--role', 'viewer', '--password-stdin', '-c', config], `${VIEWER_PASSWORD}\n`);

  const child = spawn(bin, ['run', '-c', config], { stdio: ['ignore', 'ignore', 'inherit'], detached: false });
  if (!child.pid) throw new Error('oxim run did not start');
  const baseURL = `http://127.0.0.1:${httpPort}`;
  await waitFor('the web server', async () => (await fetch(`${baseURL}/api/v1/openapi.json`)).ok);
  await waitFor('the MLLP channel', async () => {
    const ack = await sendMllp(mllpPort, SAMPLE_HL7);
    return ack.includes('MSA|AA');
  });

  const state: E2eState = {
    baseURL,
    mllpPort,
    pid: child.pid,
    dir,
    adminPassword: ADMIN_PASSWORD,
    viewerPassword: VIEWER_PASSWORD,
  };
  mkdirSync(dirname(STATE_FILE), { recursive: true });
  writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));
  child.unref();
  return state;
}

export function stop(): void {
  if (!existsSync(STATE_FILE)) return;
  const state = readState();
  try {
    process.kill(state.pid);
  } catch {
    // Already stopped.
  }
  // Give the process a moment to release its files before deleting them.
  const until = Date.now() + 5000;
  const pause = new Int32Array(new SharedArrayBuffer(4));
  while (Date.now() < until) {
    try {
      process.kill(state.pid, 0);
    } catch {
      break;
    }
    Atomics.wait(pause, 0, 0, 100);
  }
  try {
    rmSync(state.dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  } catch {
    // A leftover temporary directory is harmless.
  }
  rmSync(STATE_FILE, { force: true });
}
