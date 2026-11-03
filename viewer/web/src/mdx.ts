// Lossless segmentation of MDX for constrained rich editing. Joining every
// segment's lead + body + trail always reproduces the source byte for byte.
//   prose:     Markdown the rich editor round-trips; edited in place.
//   locked:    standard Markdown the editor would rewrite (code, tables, images); rendered, not editable.
//   mdx:       JSX, imports, exports, expressions, frontmatter; shown as inert source, never executed.
//   component: a supported component wrapping plain prose; the tags stay verbatim, the body is editable.
export type Kind = 'prose' | 'locked' | 'mdx' | 'component'
export type Segment = { kind: Kind; lead: string; body: string; trail: string; name?: string; attrs?: Record<string, string> }

export const COMPONENTS = ['Requirement', 'Decision', 'Evidence', 'Question'] as const

const blank = (l: string) => l.trim() === ''
const fence = (l: string) => /^ {0,3}(`{3,}|~{3,})/.exec(l)?.[1]
// Lines the rich editor cannot round-trip without rewriting them.
const lockedLine = (l: string) => /^ {0,3}(\||\[[^\]]+\]:|\[\^)|!\[/.test(l)
const mdxText = (s: string) => /[<{}]/.test(s.replace(/`[^`\n]*`/g, ''))

export function join(segments: Segment[]) {
  return segments.map(s => s.lead + s.body + s.trail).join('')
}

export function split(source: string): Segment[] {
  const lines = source.match(/[^\n]*\n|[^\n]+$/g) ?? []
  const blocks: Segment[] = []
  let i = 0
  const take = (kind: Kind, end: number) => { blocks.push(plain(kind, lines.slice(i, end).join(''))); i = end }
  if (/^---\s*$/.test(lines[0] ?? '')) {
    const close = lines.findIndex((l, j) => j > 0 && /^---\s*$/.test(l))
    take('mdx', close < 0 ? lines.length : close + 1)
  }
  while (i < lines.length) {
    const line = lines[i]
    let end = i + 1
    if (blank(line)) { take('prose', end); continue }
    const mark = fence(line)
    if (mark) {
      while (end < lines.length && !lines[end].trimStart().startsWith(mark)) end++
      take('locked', Math.min(end + 1, lines.length)); continue
    }
    // ponytail: tag/brace balance is counted per block, not parsed; an unbalanced block runs to EOF, which only over-protects it.
    if (/^\s*(import|export)\s/.test(line) || /^\s*[<{]/.test(line)) {
      const open = /^\s*<([A-Za-z][\w.:-]*)/.exec(line)?.[1]
      while (end < lines.length && !(blank(lines[end]) && balanced(lines.slice(i, end).join(''), open))) end++
      const text = lines.slice(i, end).join('')
      blocks.push(component(text) ?? plain('mdx', text)); i = end; continue
    }
    while (end < lines.length && !blank(lines[end]) && !fence(lines[end])) end++
    const text = lines.slice(i, end).join('')
    take(mdxText(text) ? 'mdx' : text.split('\n').some(lockedLine) ? 'locked' : 'prose', end)
  }
  // Merge prose runs and move surrounding blank lines out of the editable body.
  const out: Segment[] = []
  for (const b of blocks) {
    const last = out[out.length - 1]
    if (b.kind === 'prose' && last?.kind === 'prose') last.body += b.body
    else out.push(b)
  }
  return out.map(s => s.kind === 'prose' ? trim(s) : s)
}

const plain = (kind: Kind, body: string): Segment => ({ kind, lead: '', body, trail: '' })
function trim(s: Segment): Segment {
  const lead = /^\s*/.exec(s.body)![0], trail = /\s*$/.exec(s.body.slice(lead.length))![0]
  return { ...s, lead: s.lead + lead, trail: trail + s.trail, body: s.body.slice(lead.length, s.body.length - trail.length) }
}

function balanced(text: string, name?: string) {
  const braces = (text.match(/\{/g)?.length ?? 0) - (text.match(/\}/g)?.length ?? 0)
  if (braces > 0) return false
  if (!name) return true
  const esc = name.replace(/\./g, '\\.')
  const opens = text.match(new RegExp(`<${esc}(?=[\\s/>]|$)`, 'gm'))?.length ?? 0
  const closes = text.match(new RegExp(`</${esc}\\s*>`, 'g'))?.length ?? 0
  const selfClosing = text.match(new RegExp(`<${esc}(\\s[^<>]*)?/>`, 'g'))?.length ?? 0
  return opens - closes - selfClosing <= 0
}

// A supported component with literal string attributes, tags on their own lines and a plain-prose body.
function component(text: string): Segment | undefined {
  const m = /^(<([A-Z]\w*)((?:\s+[\w-]+="[^"{}<>]*")*)\s*>\n)([\s\S]*?)(\n<\/\2>\s*)$/.exec(text)
  if (!m || !(COMPONENTS as readonly string[]).includes(m[2])) return
  const inner = split(m[4])
  if (inner.some(s => s.kind !== 'prose')) return
  const attrs: Record<string, string> = {}
  for (const [, k, v] of m[3].matchAll(/([\w-]+)="([^"]*)"/g)) attrs[k] = v
  return trim({ kind: 'component', name: m[2], attrs, lead: m[1], body: m[4], trail: m[5] })
}
