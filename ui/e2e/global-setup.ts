import { start } from './harness';

export default async function globalSetup(): Promise<void> {
  await start();
}
