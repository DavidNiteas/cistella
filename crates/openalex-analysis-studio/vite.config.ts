import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  // bin_test loads dist/index.html from the filesystem next to the exe, so
  // emitted asset URLs must be relative instead of root-absolute (/assets/...).
  base: './',
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: '127.0.0.1',
  },
});
