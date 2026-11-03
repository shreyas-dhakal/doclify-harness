// Run with `node --test tests/mdx.test.ts` (Node strips the types).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { join, split } from '../src/mdx.ts'

const kinds = (s: string) => split(s).map(x => x.kind)

test('round-trips every input byte for byte', () => {
  for (const s of ['', 'a', '# T\n\ntext\n', '---\ntitle: x\n---\n\n# A\n', 'import X from "y"\n\n<X a={1}>\n\nhi\n\n</X>\n\ntail', '```js\n<a>\n```\n', '<Decision id="d1">\n\nUse **guest** checkout.\n\n</Decision>\n', 'x {1} y\n\n> quote\n', '<Open>\n\nnever closed\n']) {
    assert.equal(join(split(s)), s)
  }
})

test('classifies blocks', () => {
  assert.deepEqual(kinds('# T\n\n> quote\n\n- a\n  - b\n'), ['prose'])
  assert.deepEqual(kinds('---\na: 1\n---\nbody'), ['mdx', 'prose'])
  assert.deepEqual(kinds('a\n\n```\n<x>\n```\n\nb'), ['prose', 'locked', 'prose'])
  assert.deepEqual(kinds('a\n\n| x |\n|---|\n\nb'), ['prose', 'locked', 'prose'])
  assert.deepEqual(kinds('a\n\ntext {expr}\n\nb'), ['prose', 'mdx', 'prose'])
  assert.deepEqual(kinds('export const a = {\n\n  b: 1\n}\n\nok'), ['mdx', 'prose'])
  assert.deepEqual(kinds('<Chart>\n\n<Inner/>\n\n</Chart>\n\nok'), ['mdx', 'prose'])
  assert.deepEqual(kinds('<Open>\n\nnever closed\n'), ['mdx'])
})

test('supported components expose an editable body and keep tags verbatim', () => {
  const [c] = split('<Decision id="d1" status="proposed">\n\nUse guest checkout.\n\n</Decision>\n')
  assert.equal(c.kind, 'component')
  assert.equal(c.name, 'Decision')
  assert.deepEqual(c.attrs, { id: 'd1', status: 'proposed' })
  assert.equal(c.body, 'Use guest checkout.')
  assert.equal(c.lead, '<Decision id="d1" status="proposed">\n\n')
  assert.equal(kinds('<Decision id={x}>\nhi\n</Decision>')[0], 'mdx')
  assert.equal(kinds('<Decision>\n<b>x</b>\n</Decision>')[0], 'mdx')
  assert.equal(kinds('<Script>\nhi\n</Script>')[0], 'mdx')
})
