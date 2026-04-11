import { defineConfig } from 'vite'
import tailwindcss from '@tailwindcss/vite'
import { resolve } from 'node:path'

const wasmPkg = resolve(__dirname, '../../crates/nteract-predicate/pkg')

export default defineConfig({
  base: '/',
  plugins: [tailwindcss()],
  resolve: {
    alias: {
      'nteract-predicate': wasmPkg,
    },
  },
  build: {
    rolldownOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
        notebook: resolve(__dirname, 'notebook.html'),
      },
    },
  },
})
