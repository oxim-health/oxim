import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// `npm run dev` proxies the API to a local `oxim run` (default port 8080;
// override with OXIM_URL). The production build is served by OXIM itself
// under a strict Content-Security-Policy: no inline scripts or styles, so
// everything is emitted as separate files.
const target = process.env.OXIM_URL ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [svelte()],
  build: {
    target: 'es2022',
    outDir: 'dist',
    emptyOutDir: true,
    assetsInlineLimit: 0,
    cssCodeSplit: false,
    sourcemap: false,
    modulePreload: { polyfill: false },
  },
  server: {
    proxy: {
      '/api': { target, changeOrigin: false },
      '/metrics': { target, changeOrigin: false },
    },
  },
  test: {
    include: ['tests/**/*.test.ts'],
    environment: 'node',
  },
});
