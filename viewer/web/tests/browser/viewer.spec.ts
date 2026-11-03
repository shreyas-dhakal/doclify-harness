import { test, expect, type Page } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { GUIDE } from './setup'

const lineEnd = process.platform === 'darwin' ? 'Meta+ArrowRight' : 'End'
const disk = (name: string) => join(process.env.VIEWER_REPO!, 'docs/doclify', name)
async function openDoc(page: Page, name: string) {
  await page.goto(process.env.VIEWER_URL!)
  await page.getByRole('button', { name, exact: true }).click()
  await expect(page.getByRole('heading', { level: 1, name, exact: true })).toBeFocused()
}
async function noViolations(page: Page) {
  const { violations } = await new AxeBuilder({ page }).analyze()
  expect(violations.map(v => `${v.id}: ${v.nodes.map(n => n.target).join(', ')}`)).toEqual([])
}

test('overview, reading, rich editing and source modes pass axe', async ({ page }) => {
  await page.goto(process.env.VIEWER_URL!)
  await noViolations(page)
  await openDoc(page, 'guide')
  await expect(page.getByText('Unsupported MDX, preserved verbatim and not executed').first()).toBeVisible()
  await expect(page.getByRole('region', { name: 'Decision decision-1' })).toContainText('Use guest checkout.')
  await noViolations(page)
  await page.getByRole('button', { name: 'Edit' }).click()
  await expect(page.getByRole('textbox', { name: 'Decision text' })).toBeVisible()
  await noViolations(page)
  await page.getByRole('button', { name: 'Source' }).click()
  await noViolations(page)
})

test('rich edits keep unsupported MDX blocks byte for byte', async ({ page }) => {
  await openDoc(page, 'guide')
  await page.getByRole('button', { name: 'Edit' }).click()
  await expect(page.getByText('Up to date')).toBeVisible()
  await page.getByRole('textbox', { name: 'Text section 1' }).getByText('Guests can buy').click()
  await page.keyboard.press(lineEnd)
  await page.keyboard.type(' Accounts are optional.')
  await page.getByRole('textbox', { name: 'Decision text' }).getByText('checkout.').click()
  await page.keyboard.press(lineEnd)
  await page.keyboard.type(' Always.')
  await page.getByRole('button', { name: 'Save changes' }).click()
  await expect(page.getByRole('status')).toHaveText('Saved. Review status was not changed.')
  const expected = GUIDE.replace('without an account.', 'without an account. Accounts are optional.').replace('**guest** checkout.', '**guest** checkout. Always.')
  expect(readFileSync(disk('guide.mdx'), 'utf8')).toBe(expected)
})

test('a save against a changed file shows both versions and overwrites nothing', async ({ page }) => {
  await openDoc(page, 'conflict')
  await page.getByRole('button', { name: 'Source' }).click()
  await page.getByRole('textbox', { name: 'Document source' }).fill('# conflict\n\nMine.\n')
  writeFileSync(disk('conflict.md'), '# conflict\n\nTheirs.\n')
  await page.getByRole('button', { name: 'Save changes' }).click()
  const alert = page.getByRole('alert')
  await expect(alert).toContainText('Theirs.')
  await expect(alert).toContainText('Mine.')
  expect(readFileSync(disk('conflict.md'), 'utf8')).toBe('# conflict\n\nTheirs.\n')
  await noViolations(page)
  await alert.getByRole('button', { name: 'Keep my edits' }).click()
  await page.getByRole('button', { name: 'Save changes' }).click()
  await expect(page.getByRole('status')).toHaveText('Saved. Review status was not changed.')
  expect(readFileSync(disk('conflict.md'), 'utf8')).toBe('# conflict\n\nMine.\n')
})

test('review is explicit and open questions are not instructions', async ({ page }) => {
  await page.goto(process.env.VIEWER_URL!)
  const queue = page.getByRole('region', { name: /REVIEW QUEUE/ })
  await expect(queue.getByText('Guest checkout', { exact: true })).toBeVisible()
  page.once('dialog', d => d.accept())
  await queue.getByRole('button', { name: 'Accept Guest checkout' }).click()
  await expect(page.getByRole('status')).toHaveText('Review recorded: Guest checkout is accepted.')
  await expect(queue.getByText('Guest checkout', { exact: true })).toHaveCount(0)
  const stored = JSON.parse(readFileSync(join(process.env.VIEWER_REPO!, '.doclify/records/decision-1/00000000000000000002.json'), 'utf8'))
  expect(stored.record.review).toBe('accepted')
  await page.locator('summary', { hasText: 'Saved carts' }).click()
  await expect(page.getByText('Open question, not an instruction')).toBeVisible()
})

test('keyboard: skip link, document navigation and Ctrl+S save', async ({ page }) => {
  await page.goto(process.env.VIEWER_URL!)
  await page.keyboard.press('Tab')
  await expect(page.getByRole('link', { name: 'Skip to document' })).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(page.locator('main')).toBeFocused()
  await page.getByRole('button', { name: 'keyboard' }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { level: 1, name: 'keyboard', exact: true })).toBeFocused()
  await page.getByRole('button', { name: 'Source' }).press('Enter')
  await page.getByRole('textbox', { name: 'Document source' }).fill('# keyboard\n\nSaved by shortcut.\n')
  await page.keyboard.press('ControlOrMeta+s')
  await expect(page.getByRole('status')).toHaveText('Saved. Review status was not changed.')
  expect(readFileSync(disk('keyboard.md'), 'utf8')).toBe('# keyboard\n\nSaved by shortcut.\n')
})
