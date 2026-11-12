// Drive the desktop UI (mock backend) in headless Chrome. Reads one command
// per line from stdin; see SKILL.md for the command list.
//   node driver.mjs [url] < script
// Env: SHOTS (output dir), SCALE (device pixel ratio, default 2 = Retina),
// VIEWPORT (WxH, default 1400x900), KEEP_GOING=1 (log a failed step and go on,
// for tours run against older branches), HEADED=1.
import { mkdirSync, writeFileSync } from 'node:fs'
import { createInterface } from 'node:readline'
import { chromium } from 'playwright-core'

const url = process.argv[2] ?? 'http://localhost:5199/?mock'
const out = process.env.SHOTS ?? 'shots'
mkdirSync(out, { recursive: true })

const size = (wxh) => {
  const [width, height] = wxh.split('x').map(Number)
  return { width, height }
}
const browser = await chromium.launch({ channel: 'chrome', headless: !process.env.HEADED })
const p = await browser.newPage({
  viewport: size(process.env.VIEWPORT ?? '1400x900'),
  deviceScaleFactor: Number(process.env.SCALE ?? 2)
})
// A tour run against an older branch hits steps that can't work there; fail them fast.
if (process.env.KEEP_GOING) p.setDefaultTimeout(5000)
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

// `ss name -- what it shows` lines collect here; written to $SHOTS/manifest.json.
const manifest = []
const shot = async (name, desc, el) => {
  const path = `${out}/${name}.png`
  await (el ?? p).screenshot({ path })
  manifest.push({ file: `${name}.png`, desc, viewport: p.viewportSize() })
  console.log('screenshot', path)
}
// Split `a b -- rest` into ([a, b], 'rest').
const described = (args) => {
  const i = args.indexOf('--')
  return i < 0 ? [args, ''] : [args.slice(0, i), args.slice(i + 1).join(' ')]
}

const commands = {
  // Fill the connect form and press Connect without waiting for success (error states).
  'connect-try': async (host) => {
    await p.getByPlaceholder(/user@devbox/).fill(host)
    await p.getByRole('button', { name: 'Connect', exact: true }).click()
  },
  // Connect the mock host (any name; "fail" in it fails, "udp-blocked" falls back to SSH).
  connect: async (host = 'devbox') => {
    await p.getByPlaceholder(/user@devbox/).fill(host)
    await p.getByRole('button', { name: 'Connect', exact: true }).click()
    await p.getByText('1 host connected').waitFor()
  },
  // Select a worktree in the sidebar by its visible text.
  worktree: async (...name) => p.getByText(name.join(' ')).first().click(),
  // Hover a sidebar worktree row (by its visible text) to show its hover details.
  'hover-worktree': async (...name) => p.getByText(name.join(' ')).first().hover(),
  // Right panel: Files | Changes | Branch | Ports.
  right: async (name) => p.getByRole('tab', { name: new RegExp(`^${name}`, 'i') }).click(),
  'click-file': async (...name) => row(name.join(' ')).click(),
  'dblclick-file': async (...name) => row(name.join(' ')).dblclick(),
  'rclick-file': async (...name) => row(name.join(' ')).click({ button: 'right' }),
  // ⌘-click (add to / remove from the selection) or ⇧-click (range).
  'cmd-click-file': async (...name) => row(name.join(' ')).click({ modifiers: ['Meta'] }),
  'shift-click-file': async (...name) => row(name.join(' ')).click({ modifiers: ['Shift'] }),
  // Drag a Files row onto another (`drag-file a -> b`; a folder, or a file: its folder).
  'drag-file': async (...args) => {
    const [from, to] = args.join(' ').split(' -> ')
    const a = await row(from).boundingBox()
    const b = await row(to).boundingBox()
    await p.mouse.move(a.x + 20, a.y + a.height / 2)
    await p.mouse.down()
    await p.mouse.move(a.x + 30, a.y + a.height / 2 + 6, { steps: 3 })
    await p.mouse.move(b.x + 30, b.y + b.height / 2, { steps: 8 })
    await p.mouse.up()
  },
  // Drag a resize handle (role=separator) by its label, `dx` px (negative = left).
  'drag-separator': async (...args) => {
    const dx = Number(args.pop())
    const b = await p.getByRole('separator', { name: args.join(' ') }).boundingBox()
    const x = b.x + b.width / 2
    const y = b.y + b.height / 2
    await p.mouse.move(x, y)
    await p.mouse.down()
    await p.mouse.move(x + dx, y, { steps: 8 })
    await p.mouse.up()
  },
  'dblclick-separator': async (...name) =>
    p.getByRole('separator', { name: name.join(' ') }).dblclick(),
  // An item of the open context menu, by its text.
  menu: async (...label) =>
    p.getByRole('menuitem', { name: new RegExp(`^${esc(label.join(' '))}`) }).click(),
  type: async (...text) => p.keyboard.type(text.join(' ')),
  'click-change': async (name) => change(name).click(),
  'dblclick-change': async (name) => change(name).dblclick(),
  // ⌘- / ⇧-click a Changes row (multi-selection), hover one (shows its buttons).
  'meta-click-change': async (name) => change(name).click({ modifiers: ['Meta'] }),
  'shift-click-change': async (name) => change(name).click({ modifiers: ['Shift'] }),
  'hover-change': async (name) => change(name).hover(),
  // A button by its accessible name (icon buttons: their tooltip label), exact.
  'click-button': async (...name) =>
    p.getByRole('button', { name: name.join(' '), exact: true }).first().click(),
  // An item of the open dropdown / context menu, by the start of its text.
  'menu-item': async (...text) =>
    p.getByRole('menuitem', { name: new RegExp(`^${esc(text.join(' '))}`) }).first().click(),
  // Type into the Changes tab's commit message box.
  'commit-message': async (...text) => p.getByLabel('Commit message').fill(text.join(' ')),
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
  // `ss name [-- description]`; the description goes to manifest.json.
  ss: async (...args) => {
    const [[name = 'shot'], desc] = described(args)
    await shot(name, desc)
  },
  // Screenshot one element (first match of a CSS selector), e.g. `ss-el sidebar aside -- desc`.
  'ss-el': async (...args) => {
    const [[name, ...selector], desc] = described(args)
    await shot(name, desc, p.locator(selector.join(' ')).first())
  },
  // Run JS in the page with the app's own modules in scope (Vite dev serves /src):
  // `store` (state/store.ts), `prompts` (state/prompts.ts), `s` = store state.
  // e.g. `js store.useStore.setState({ settings: 'hosts' })`
  js: async (...code) => {
    const r = await p.evaluate(async (code) => {
      const store = await import('/src/state/store.ts')
      const prompts = await import('/src/state/prompts.ts')
      const s = store.useStore.getState()
      return eval(code)
    }, code.join(' '))
    if (r !== undefined) console.log(typeof r === 'string' ? r : JSON.stringify(r))
  },
  // Load another URL, relative to the start URL (e.g. `goto /?about`).
  goto: async (to) => p.goto(new URL(to, url).href),
  // Start over: forget saved hosts and prefs, reload the start URL (connect screen).
  fresh: async () => {
    await p.evaluate(() => localStorage.clear())
    await p.goto(url)
  },
  // `toasts off` hides toast notifications (they cover the right panel), `toasts on` shows them.
  toasts: async (state) =>
    p.evaluate((hide) => {
      let el = document.getElementById('driver-toasts')
      if (!el) {
        el = document.createElement('style')
        el.id = 'driver-toasts'
        document.head.append(el)
      }
      el.textContent = hide ? '[data-sonner-toaster] { display: none !important }' : ''
    }, state === 'off'),
  // Hover any element by its exact visible text.
  'hover-text': async (...text) => p.getByText(text.join(' '), { exact: true }).first().hover(),
  // Click the first match of a CSS selector (e.g. `click .xterm`).
  click: async (...selector) => p.locator(selector.join(' ')).first().click(),
  // Scroll the mouse wheel over an element (CSS selector): `scroll -800 .xterm`.
  scroll: async (dy, ...selector) => {
    await p.locator(selector.join(' ')).first().hover()
    await p.mouse.wheel(0, Number(dy))
  },
  // Resize the window, e.g. `viewport 1000x700`.
  viewport: async (wxh) => p.setViewportSize(size(wxh)),
  // Hover a button by its accessible name (shows its tooltip).
  'hover-button': async (...name) =>
    p.getByRole('button', { name: name.join(' '), exact: true }).first().hover(),
  // Right-click any element by its exact visible text (e.g. a sidebar project row).
  rclick: async (...text) =>
    p.getByText(text.join(' '), { exact: true }).first().click({ button: 'right' }),
  // Click any element by its exact visible text (nav items that aren't buttons).
  'click-text': async (...text) => p.getByText(text.join(' '), { exact: true }).first().click(),
  // Move the mouse out of the way (drops hover states and tooltips).
  unhover: async () => p.mouse.move(0, 0)
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
    if (!process.env.KEEP_GOING) break
  }
}
if (manifest.length) writeFileSync(`${out}/manifest.json`, `${JSON.stringify(manifest, null, 2)}\n`)
for (const e of errors) console.log('pageerror', e)
await browser.close()
// A page exception fails the run even when every command went through.
process.exit(failed || errors.length > 0 ? 1 : 0)
