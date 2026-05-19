import { defineConfig } from 'vite';

export default defineConfig({
  build: {
    assetsDir: 'assets',
    manifest: true,
    outDir: 'dist',
    rollupOptions: {
      output: {
        entryFileNames: 'assets/index.js',
        assetFileNames: 'assets/[name][extname]',
        chunkFileNames: 'assets/[name].js'
      }
    }
  }
});
