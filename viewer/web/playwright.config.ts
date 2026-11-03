import { defineConfig } from '@playwright/test'

// Serves the committed dist assets through the Rust viewer, as end users get them. Run `npm run build` first.
export default defineConfig({
  testDir: 'tests/browser',
  globalSetup: './tests/browser/setup.ts',
  workers: 1,
  use: { browserName: 'chromium' },
})
