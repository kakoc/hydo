import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// VITE_VARIANT=buggy builds the variant with the planted bugs (src/api.js, src/App.jsx);
// build.sh writes it to dist-buggy and the fixed one to dist-fixed.
export default defineConfig({ plugins: [react()] });
