// Prepends a "generated file" notice to the TypeScript types that
// openapi-typescript writes, so nobody edits them by hand.
import { readFileSync, writeFileSync } from 'node:fs';

const NOTICE =
  '// Generated from ui/openapi.json by `npm run gen:api` (openapi-typescript).\n' +
  '// Do not edit. The Rust test `cargo test -p oxim-server --test openapi`\n' +
  '// keeps openapi.json in step with the server.\n\n';

for (const path of process.argv.slice(2)) {
  const text = readFileSync(path, 'utf8').replace(/\r\n/g, '\n');
  const body = text.startsWith('// Generated from') ? text.slice(text.indexOf('\n\n') + 2) : text;
  writeFileSync(path, NOTICE + body);
}
