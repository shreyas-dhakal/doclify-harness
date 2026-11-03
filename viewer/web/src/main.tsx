import React, { useEffect, useRef, useState } from 'react'
import { createRoot } from 'react-dom/client'
import ReactMarkdown from 'react-markdown'
import { MDXEditor, headingsPlugin, listsPlugin, quotePlugin, thematicBreakPlugin, linkPlugin, markdownShortcutPlugin, toolbarPlugin, UndoRedo, BoldItalicUnderlineToggles, ListsToggle } from '@mdxeditor/editor'
import '@mdxeditor/editor/style.css'
import './style.css'
import { join, split, type Segment } from './mdx'

type SearchHit = { kind: 'document' | 'record'; path: string; title: string }
type Doc = { path: string; content: string; revision: string }
type Provenance = { source: string; excerpt: string | null }
type DocRecord = { id: string; version: number; kind: string; title: string; statement: string; rationale: string; review: string; implementation: string; experiment_outcome: string | null; provenance: Provenance[]; evidence: string[]; supersedes: number | null }
const token = location.hash.slice(1) || sessionStorage.getItem('doclify-token') || ''
if (location.hash) sessionStorage.setItem('doclify-token', token)
history.replaceState(null, '', location.pathname)
const label = (p: string) => p.replace(/\.(md|mdx)$/, '').replace(/[-_]/g, ' ')
const words = (s: string) => s.replace(/_/g, ' ')
const write = (url: string, method: string, body: unknown) => fetch(url, { method, headers: { 'content-type': 'application/json', 'x-doclify-token': token }, body: JSON.stringify(body) })
const SESSION = 'This viewer session is no longer authorized. Run /docs again to open a fresh session; your edits remain here until you leave.'
// Records that must stay discoverable without reading as settled instructions.
const notInstruction = (r: DocRecord) => r.kind === 'question' ? 'Open question' : r.experiment_outcome === 'abandoned' ? 'Abandoned experiment' : r.review === 'rejected' ? 'Rejected' : r.review === 'superseded' ? 'Superseded' : r.review === 'proposed' ? 'Awaiting review' : ''

function Markdown({ text }: { text: string }) { return <ReactMarkdown skipHtml>{text}</ReactMarkdown> }
function Raw({ text }: { text: string }) { return <figure className="raw"><figcaption>Unsupported MDX, preserved verbatim and not executed</figcaption><pre>{text}</pre></figure> }
function Callout({ s, children }: { s: Segment; children: React.ReactNode }) {
  return <section className={'callout callout-' + s.name!.toLowerCase()} aria-label={`${s.name}${s.attrs?.id ? ' ' + s.attrs.id : ''}`}>
    <div className="callout-head"><strong>{s.name}</strong>{s.attrs?.id && <code>{s.attrs.id}</code>}{s.attrs?.status && <span className="badge">{words(s.attrs.status)}</span>}</div>{children}</section>
}
function Rendered({ text }: { text: string }) {
  return <>{split(text).map((s, i) => s.kind === 'mdx' ? <Raw key={i} text={s.body} /> : s.kind === 'component' ? <Callout key={i} s={s}><Markdown text={s.body} /></Callout> : <Markdown key={i} text={s.body} />)}</>
}
function Editable({ body, name, onChange }: { body: string; name: string; onChange: (md: string) => void }) {
  // The editing surface takes its accessible name from this translation key.
  return <div className="editable"><MDXEditor translation={(key, fallback) => key === 'contentArea.editableMarkdown' ? name : fallback} markdown={body} onChange={(md, initial) => { if (!initial) onChange(md) }} contentEditableClassName="editor" plugins={[headingsPlugin(), listsPlugin(), quotePlugin(), thematicBreakPlugin(), linkPlugin(), markdownShortcutPlugin(), toolbarPlugin({ toolbarContents: () => <><UndoRedo /><BoldItalicUnderlineToggles /><ListsToggle /></> })]} /></div>
}
// Only prose, and the bodies of supported components, reach the rich editor; everything else is joined back untouched.
function RichEditor({ segments, onChange }: { segments: Segment[]; onChange: (s: Segment[]) => void }) {
  const update = (i: number, body: string) => onChange(segments.map((s, j) => j === i ? { ...s, body } : s))
  let n = 0
  return <>{segments.map((s, i) => s.kind === 'prose' ? (s.body || segments.length === 1) && <Editable key={i} name={`Text section ${++n}`} body={s.body} onChange={md => update(i, md)} />
    : s.kind === 'component' ? <Callout key={i} s={s}><Editable name={`${s.name} text`} body={s.body} onChange={md => update(i, md)} /></Callout>
    : s.kind === 'locked' ? <div key={i} className="locked" role="group" aria-label="Locked block"><p className="locked-note">Locked block. Edit it in Source mode.</p><Markdown text={s.body} /></div>
    : <Raw key={i} text={s.body} />)}</>
}

function App() {
  const [files, setFiles] = useState<string[]>([]), [docs, setDocs] = useState<Doc[]>([]), [selected, setSelected] = useState<Doc | null>(null)
  const [text, setText] = useState(''), [mode, setMode] = useState<'read' | 'rich' | 'source'>('read'), [segments, setSegments] = useState<Segment[]>([])
  const [query, setQuery] = useState(''), [records, setRecords] = useState<DocRecord[]>([]), [message, setMessage] = useState(''), [busy, setBusy] = useState(false), [conflict, setConflict] = useState<Doc | null>(null)
  const [hits, setHits] = useState<SearchHit[]>([])
  useEffect(() => {
    if (!query.trim()) { setHits([]); return }
    const controller = new AbortController()
    const timer = setTimeout(() => { fetch('/api/search?q=' + encodeURIComponent(query), { signal: controller.signal }).then(r => { if (!r.ok) throw new Error(); return r.json() }).then(setHits).catch(e => { if (e.name !== 'AbortError') setMessage('Search failed. Check that the viewer is still running.') }) }, 150)
    return () => { clearTimeout(timer); controller.abort() }
  }, [query, docs, records])
  const heading = useRef<HTMLHeadingElement>(null), conflictHeading = useRef<HTMLHeadingElement>(null)
  const dirty = !!selected && text !== selected.content
  const loadRecords = () => fetch('/api/records').then(r => r.json()).then(setRecords)
  useEffect(() => { Promise.all([fetch('/api/documents').then(r => r.json()), loadRecords()]).then(([paths]: [string[], unknown]) => { setFiles(paths); return Promise.all(paths.map(p => fetch('/api/document?path=' + encodeURIComponent(p)).then(r => r.json()))) }).then(setDocs).catch(() => setMessage('Could not load documentation. Check that the viewer is still running, or run /docs again.')) }, [])
  useEffect(() => { const handler = (e: BeforeUnloadEvent) => { if (dirty) e.preventDefault() }; window.addEventListener('beforeunload', handler); return () => window.removeEventListener('beforeunload', handler) }, [dirty])
  useEffect(() => { const handler = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key === 's') { e.preventDefault(); if (dirty && !busy) save() } }; window.addEventListener('keydown', handler); return () => window.removeEventListener('keydown', handler) })
  useEffect(() => { heading.current?.focus() }, [selected?.path])
  useEffect(() => { conflictHeading.current?.focus() }, [conflict])
  function switchMode(next: typeof mode) { if (next === 'rich') setSegments(split(text)); setMode(next) }
  function replace(doc: Doc) { setSelected(doc); setDocs(old => old.map(d => d.path === doc.path ? doc : d)) }
  function open(doc: Doc) { if (dirty && !confirm('Discard unsaved changes?')) return; setSelected(doc); setText(doc.content); setMode('read'); setMessage(''); setConflict(null) }
  async function save() {
    if (!selected) return; setBusy(true); setMessage('')
    try {
      const r = await write('/api/document', 'PUT', { path: selected.path, content: text, revision: selected.revision })
      if (r.status === 409) { setConflict(await r.json()); return }
      if (r.status === 403) { setMessage(SESSION); return }
      if (!r.ok) throw new Error()
      replace(await r.json()); setConflict(null); setMessage('Saved. Review status was not changed.')
    } catch { setMessage('Save failed. Check that the viewer is still running and try again; your edits remain here.') } finally { setBusy(false) }
  }
  function keepMine() { replace(conflict!); setConflict(null); setMessage('Your edits will replace the latest version when you save.') }
  function useLatest() { replace(conflict!); setText(conflict!.content); setMode('read'); setConflict(null); setMessage('Loaded the latest version. Your edits were discarded.') }
  async function review(record: DocRecord, action: 'accept' | 'reject' | 'supersede') {
    if (!confirm(`${action[0].toUpperCase() + action.slice(1)} "${record.title}"? This appends a new reviewed version.`)) return
    const r = await write('/api/review', 'POST', { id: record.id, version: record.version, action }).catch(() => null)
    if (r?.ok) { const updated: DocRecord = await r.json(); setRecords(old => old.map(x => x.id === updated.id ? updated : x)); setMessage(`Review recorded: ${record.title} is ${words(updated.review)}.`); return }
    if (r?.status === 403) { setMessage(SESSION); return }
    await loadRecords().catch(() => {}); setMessage('Review was not recorded because the proposal changed or the viewer stopped. The queue now shows the latest versions.')
  }
  const filtered = query.trim() ? docs.filter(d => hits.some(h => h.kind === 'document' && h.path === d.path)) : docs
  const matchingRecords = query.trim() ? records.filter(r => hits.some(h => h.kind === 'record' && h.path === r.id)) : records
  const proposed = records.filter(r => r.review === 'proposed')
  const status = message && <div role="status" className="notice">{message}</div>
  return <><a className="skip" href="#main">Skip to document</a><header><div className="brand"><span className="mark" aria-hidden="true">D</span><span>Doclify <small> / Documentation</small></span></div><span className="local"><span className="dot" aria-hidden="true" /> Local workspace</span></header>
    <div className="layout"><aside className="nav" aria-label="Library"><h2>Library</h2><label className="search"><span>Search documentation and records</span><input type="search" value={query} onChange={e => setQuery(e.target.value)} placeholder="Search titles and text" /></label>
      <div className="section-title">DOCUMENTS <span>{filtered.length}</span></div>
      <nav aria-label="Documentation">{filtered.map(d => <button key={d.path} className={'nav-item ' + (selected?.path === d.path ? 'active' : '')} onClick={() => open(d)} aria-current={selected?.path === d.path ? 'page' : undefined}><span className="file-icon" aria-hidden="true">▤</span>{label(d.path)}</button>)}</nav>
      {!files.length && <p className="muted">No documents yet. Generate documentation in <code>docs/doclify/</code> to see it here.</p>}{files.length > 0 && !filtered.length && <p className="muted">No matching documents.</p>}</aside>
      <main id="main" tabIndex={-1}>{!token && <div className="notice">This page was opened without a session token, so editing and review are disabled. Run /docs to open it with one.</div>}{!selected && status}
        <div className="main-top"><span>DOCUMENTATION <span className="slash">/</span> {selected ? label(selected.path) : 'OVERVIEW'}</span><span>{dirty ? 'Unsaved changes' : selected ? 'Up to date' : ''}</span></div>
        {selected ? <><div className="title-row"><div><p className="eyebrow">WORKSPACE DOCUMENT</p><h1 ref={heading} tabIndex={-1}>{label(selected.path)}</h1><p className="path">{selected.path}</p></div>
          <div className="actions" role="group" aria-label="Document mode">{(['read', 'rich', 'source'] as const).map(m => <button key={m} aria-pressed={mode === m} className={mode === m ? 'chosen' : ''} onClick={() => switchMode(m)}>{{ read: 'Read', rich: 'Edit', source: 'Source' }[m]}</button>)}
            <button className="primary" disabled={!dirty || busy || !token} onClick={save} aria-keyshortcuts="Control+S Meta+S">{busy ? 'Saving…' : 'Save changes'}</button></div></div>
          {status}
          {conflict && <section className="conflict" role="alert" aria-labelledby="conflict-title"><h2 id="conflict-title" ref={conflictHeading} tabIndex={-1}>This document changed in another tab or on disk</h2><p>Nothing was overwritten. Compare the versions, merge in Source mode if needed, then choose how to continue.</p>
            <div className="compare"><div><h3>Latest saved version</h3><pre>{conflict.content}</pre></div><div><h3>Your version</h3><pre>{text}</pre></div></div>
            <button className="primary" onClick={keepMine}>Keep my edits</button> <button onClick={useLatest}>Use latest and discard mine</button> <button onClick={() => switchMode('source')}>Merge in Source</button></section>}
          {mode === 'rich' && segments.some(s => s.kind === 'mdx' || s.kind === 'locked') && <p className="notice">Some blocks are locked because rich editing would rewrite them. They are saved exactly as they are; use Source mode to change them.</p>}
          <article className="document" aria-label="Document">{mode === 'source' ? <textarea aria-label="Document source" value={text} onChange={e => setText(e.target.value)} spellCheck={false} />
            : mode === 'rich' ? <RichEditor key={selected.path} segments={segments} onChange={s => { setSegments(s); setText(join(s)) }} /> : <Rendered text={text} />}</article></>
          : <div className="empty"><div className="empty-icon" aria-hidden="true">▤</div><h1>Your workspace, in context.</h1><p>Select a document to read or edit. Decisions, evidence, and open questions stay close to the work.</p>{!files.length && <p>Documentation will appear here once it has been generated.</p>}</div>}</main>
      <aside className="context" aria-label="Context"><h2>Context</h2><p className="muted">Review and implementation are tracked separately.</p>
        <section className="context-section" aria-labelledby="queue-title"><h3 id="queue-title" className="section-title">REVIEW QUEUE <span>{proposed.length}</span></h3>
          {proposed.length ? proposed.map(r => <div className="record" key={r.id}><strong>{r.title}</strong><div><span className="badge">Proposed {words(r.kind)}</span> <span className="muted">Implementation: {words(r.implementation)}</span></div><p>{r.statement}</p><p className="muted">{r.rationale}</p>
            <div className="review-actions">{(['accept', 'reject', 'supersede'] as const).map(a => <button key={a} disabled={!token} onClick={() => review(r, a)} aria-label={`${a[0].toUpperCase() + a.slice(1)} ${r.title}`}>{a[0].toUpperCase() + a.slice(1)}</button>)}</div></div>) : <p className="muted">No proposed records found.</p>}
          <p className="muted small">Accept, reject, or supersede each proposal explicitly. Saving a document does not accept a decision.</p></section>
        <section className="context-section" aria-labelledby="records-title"><h3 id="records-title" className="section-title">EVIDENCE &amp; RECORDS <span>{records.length}</span></h3>
          {matchingRecords.length ? matchingRecords.map(r => <details key={r.id}><summary>{r.title} <span className="muted">· {words(r.kind)} · {words(r.review)}</span></summary>
            {notInstruction(r) && <p className="badge">{notInstruction(r)}, not an instruction</p>}<p>{r.statement}</p><p className="muted">{r.rationale}</p>
            <dl><dt>Review</dt><dd>{words(r.review)} (version {r.version})</dd><dt>Implementation</dt><dd>{words(r.implementation)}</dd>{r.experiment_outcome && <><dt>Experiment</dt><dd>{words(r.experiment_outcome)}</dd></>}</dl>
            {r.evidence.length > 0 && <><h4>Evidence</h4><ul>{r.evidence.map((e, i) => <li key={i}><code>{e}</code></li>)}</ul></>}
            {r.provenance.length > 0 && <><h4>Provenance</h4><ul>{r.provenance.map((p, i) => <li key={i}><code>{p.source}</code>{p.excerpt && <blockquote>{p.excerpt}</blockquote>}</li>)}</ul></>}</details>) : <p className="muted">{query.trim() ? "No matching records." : "Evidence will appear when records are available."}</p>}</section></aside></div></>
}
createRoot(document.getElementById('root')!).render(<App />)
