import { resolve } from 'node:path'
import { defineConfig } from 'vite'
import react, { reactCompilerPreset } from '@vitejs/plugin-react'
import babel from '@rolldown/plugin-babel'

// The UI of the Tauri app (src-tauri). `tauri dev` runs `vite` on :5173; `tauri build` bundles out/renderer.
export default defineConfig({
  root: 'src/renderer',
  base: './',
  clearScreen: false,
  resolve: { alias: { '@': resolve('src/renderer/src') } },
  // React Compiler: auto-memoizes components/hooks, so re-renders skip unchanged subtrees
  plugins: [react(), babel({ presets: [reactCompilerPreset()] })],
  server: { port: 5173, strictPort: true, host: '127.0.0.1' },
  build: {
    outDir: resolve('out/renderer'),
    emptyOutDir: true,
    // WebView2 is evergreen Chromium; the macOS build runs on WebKit (Safari 16+)
    target: ['chrome120', 'safari16'],
    chunkSizeWarningLimit: 4096
  }
})
