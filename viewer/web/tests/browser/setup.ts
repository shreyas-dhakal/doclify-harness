import { spawn, execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createInterface } from 'node:readline'

export const GUIDE = `---
title: Guide
---

import Chart from "./chart"

# Checkout

Guests can buy without an account.

[Source excerpt](../../.doclify/records/decision-1/00000000000000000001.json)

<Decision id="decision-1" status="accepted">

Use **guest** checkout.

</Decision>

<Chart data={{ rate: 0.4 }}>

  odd   spacing *kept*

</Chart>

\`\`\`js
const x = <b>{1}</b>
\`\`\`

| a | b |
|---|---|

Closing *line*.
`

const record = (id: string, kind: string, title: string) => JSON.stringify({ schema_version: 1, record: { id, version: 1, kind, title, statement: `${title} statement`, rationale: `${title} rationale`, review: 'proposed', implementation: 'planned', experiment_outcome: null, provenance: [{ source: 'codex:session-1', excerpt: 'we agreed guests can check out' }], evidence: ['codex:session-1', 'src/checkout.ts'], supersedes: null } })

export default async function setup() {
  const repo = mkdtempSync(join(tmpdir(), 'doclify-viewer-'))
  mkdirSync(join(repo, 'docs/doclify'), { recursive: true })
  writeFileSync(join(repo, 'docs/doclify/guide.mdx'), GUIDE)
  for (const name of ['notes.md', 'conflict.md', 'keyboard.md']) writeFileSync(join(repo, 'docs/doclify', name), `# ${name}\n\nPlain text.\n`)
  for (const [id, kind, title] of [['decision-1', 'decision', 'Guest checkout'], ['question-1', 'question', 'Saved carts']]) {
    mkdirSync(join(repo, '.doclify/records', id), { recursive: true })
    writeFileSync(join(repo, '.doclify/records', id, '00000000000000000001.json'), record(id, kind, title))
  }
  const manifest = join(import.meta.dirname, '../../../Cargo.toml')
  execFileSync('cargo', ['build', '--quiet', '--example', 'serve', '--manifest-path', manifest], { stdio: 'inherit' })
  const server = spawn('cargo', ['run', '--quiet', '--example', 'serve', '--manifest-path', manifest, '--', repo], { stdio: ['ignore', 'pipe', 'inherit'] })
  const [url] = await createInterface({ input: server.stdout })[Symbol.asyncIterator]().next().then(r => [r.value as string])
  process.env.VIEWER_URL = url
  process.env.VIEWER_REPO = repo
  return () => { server.kill() }
}
