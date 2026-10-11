import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  server: { proxy: { '/api': 'http://127.0.0.1:10801', '/auth': 'http://127.0.0.1:10801' } },
  build: {
    outDir: '../crates/proxy/src/web/dist',
    emptyOutDir: true,
    cssCodeSplit: false,
    rollupOptions: { output: { entryFileNames: 'assets/app.js', assetFileNames: 'assets/[name][extname]', inlineDynamicImports: true } }
  }
});
