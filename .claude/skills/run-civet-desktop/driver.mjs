// Drive the desktop UI (mock backend) in headless Chrome. Reads one command
// per line from stdin; see SKILL.md for the command list.
//   node driver.mjs [url] < script
import { mkdirSync } from 'node:fs'
import { createInterface } from 'node:readline'
import { chromium } from 'playwright-core'

const url = process.argv[2] ?? 'http://localhost:5199/?mock'
const out = process.env.SHOTS ?? 'shots'
mkdirSync(out, { recursive: true })

const browser = await chromium.launch({ channel: 'chrome', headless: !process.env.HEADED })
const p = await browser.newPage({ viewport: { width: 1400, height: 900 } })
const errors = []
p.on('pageerror', (e) => errors.push(e.message))
await p.goto(url)

// Editor tabs (TitleBar) carry data-tab-id; the right panel's tabs are role="tab" too.
const EDITOR_TAB = '[role="tab"][data-tab-id]'
const row = (name) =>
  p.locator('button', { has: p.locator('span', { hasText: new RegExp(`^${esc(name)}$`) }) }).first()
const change = (name) =>
  p.locator('button', { has: p.locator('span', { hasText: new RegExp(`^${esc(name)}$`) }) })
    .filter({ has: p.locator('span.font-mono') })
    .first()
const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
const editorTab = (label) => p.locator(EDITOR_TAB, { hasText: label }).first()

const commands = {
  // Connect the mock host (any name; "fail" in it fails, "udp-blocked" falls back to SSH).
  connect: async (host = 'devbox') => {
    await p.getByPlaceholder(/user@devbox/).fill(host)
    await p.getByRole('button', { name: 'Connect', exact: true }).click()
    await p.getByText('1 host connected').waitFor()
  },
  // Select a worktree in the sidebar by its visible text.
  worktree: async (...name) => p.getByText(name.join(' ')).first().click(),
  // Right panel: Files | Changes | Branch | Ports.
  right: async (name) => p.getByRole('tab', { name: new RegExp(`^${name}`, 'i') }).click(),
  'click-file': async (name) => row(name).click(),
  'dblclick-file': async (name) => row(name).dblclick(),
  'click-change': async (name) => change(name).click(),
  'dblclick-change': async (name) => change(name).dblclick(),
  'click-tab': async (...label) => editorTab(label.join(' ')).click(),
  'dblclick-tab': async (...label) => editorTab(label.join(' ')).dblclick(),
  'close-tab': async (...label) =>
    editorTab(label.join(' ')).getByRole('button', { name: /^Close tab/ }).click({ force: true }),
  // Editor tab strip: label, * = italic (preview), < = active.
  tabs: async () => {
    const t = await p.$$eval(EDITOR_TAB, (els) =>
      els.map((e) => {
        const s = e.querySelector('span.whitespace-nowrap')
        const italic = s && getComputedStyle(s).fontStyle === 'italic'
        return `${s?.textContent ?? '?'}${italic ? '*' : ''}${e.dataset.active === 'true' ? '<' : ''}`
      })
    )
    console.log('tabs', JSON.stringify(t))
  },
  key: async (combo) => p.keyboard.press(combo),
  // An ⌥ shortcut (opt t, opt 1, opt Backquote). On a Mac the app only takes ⌥+key
  // when `key` is not plain ASCII (App.tsx isOptionShortcut), but Playwright's
  // Alt+t sends key "t"; send what a US layout composes instead.
  opt: async (k) => {
    const code = /^[a-z]$/i.test(k) ? `Key${k.toUpperCase()}` : /^\d$/.test(k) ? `Digit${k}` : k
    await p.evaluate((code) => {
      const init = { key: '\u2020', code, altKey: true, bubbles: true, cancelable: true }
      ;(document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent('keydown', init))
    }, code)
  },
  // Every match's text (a selector may match several elements).
  text: async (...s) =>
    console.log((await p.locator(s.join(' ') || 'body').allInnerTexts()).join('\n').slice(0, 3000)),
  eval: async (...js) => console.log(await p.evaluate(js.join(' '))),
  wait: async (ms = '300') => p.waitForTimeout(Number(ms)),
  ss: async (name = 'shot') => {
    const path = `${out}/${name}.png`
    await p.screenshot({ path })
    console.log('screenshot', path)
  }
}

let failed = false
for await (const line of createInterface({ input: process.stdin })) {
  const [cmd, ...args] = line.trim().split(/\s+/)
  if (!cmd || cmd.startsWith('#')) continue
  const fn = commands[cmd]
  if (!fn) {
    console.log(`unknown command: ${cmd} (${Object.keys(commands).join(', ')})`)
    failed = true
    continue
  }
  try {
    await fn(...args)
    // Let React settle before the next step.
    if (cmd !== 'wait') await p.waitForTimeout(150)
  } catch (e) {
    console.log(`FAILED ${line}: ${e.message.split('\n')[0]}`)
    failed = true
    break
  }
}
for (const e of errors) console.log('pageerror', e)
await browser.close()
// A page exception fails the run even when every command went through.
process.exit(failed || errors.length > 0 ? 1 : 0)
