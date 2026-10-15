import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'node:path';
export default defineConfig({ plugins: [react()], build: { rollupOptions: { input: { flyout: resolve(import.meta.dirname, 'index.html'), settings: resolve(import.meta.dirname, 'settings.html') } } } });
