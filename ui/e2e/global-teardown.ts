import { stop } from './harness';

export default async function globalTeardown(): Promise<void> {
  stop();
}
