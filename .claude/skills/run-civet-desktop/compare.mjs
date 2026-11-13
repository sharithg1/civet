// Pair up before/after tour screenshots and render a side-by-side for each one
// that changed: before | after, with the changed pixels of "after" tinted magenta. Prints one line per screenshot with the share of changed pixels.
//   node compare.mjs <before-dir> <after-dir> <out-dir>
import { existsSync, mkdirSync, readdirSync, readFileSync } from 'node:fs'
import { chromium } from 'playwright-core'

const [before, after, out] = process.argv.slice(2)
if (!out) {
  console.log('usage: node compare.mjs <before-dir> <after-dir> <out-dir>')
  process.exit(2)
}
mkdirSync(out, { recursive: true })

const png = (dir, f) =>
  existsSync(`${dir}/${f}`) ? `data:image/png;base64,${readFileSync(`${dir}/${f}`).toString('base64')}` : null
const names = [...new Set([...readdirSync(before), ...readdirSync(after)])]
  .filter((f) => f.endsWith('.png'))
  .sort()

const browser = await chromium.launch({ channel: 'chrome' })
const p = await browser.newPage()
for (const f of names) {
  const a = png(before, f)
  const b = png(after, f)
  if (!a || !b) {
    console.log(`${f.padEnd(34)} ${a ? 'only before' : 'only after (new)'}`)
    continue
  }
  // Diff on a canvas; a channel delta under 24 counts as the same (anti-aliasing, live numbers).
  const { pct, diff } = await p.evaluate(
    async ([a, b]) => {
      const load = (src) => new Promise((r) => { const i = new Image(); i.onload = () => r(i); i.src = src })
      const [ia, ib] = await Promise.all([load(a), load(b)])
      const w = Math.max(ia.width, ib.width)
      const h = Math.max(ia.height, ib.height)
      const px = (img) => {
        const c = new OffscreenCanvas(w, h).getContext('2d')
        c.drawImage(img, 0, 0)
        return c.getImageData(0, 0, w, h)
      }
      const da = px(ia).data
      const db = px(ib)
      const diff = new ImageData(w, h)
      let changed = 0
      for (let i = 0; i < da.length; i += 4) {
        const d = Math.max(...[0, 1, 2].map((k) => Math.abs(da[i + k] - db.data[i + k])))
        const hit = d >= 24
        changed += hit
        const [r, g, b] = [db.data[i], db.data[i + 1], db.data[i + 2]]
        diff.data.set(hit ? [(r + 255) / 2, g / 2, (b + 200) / 2, 255] : [r, g, b, 255], i)
      }
      const c = document.createElement('canvas')
      c.width = w
      c.height = h
      c.getContext('2d').putImageData(diff, 0, 0)
      return { pct: (100 * changed) / (w * h), diff: c.toDataURL() }
    },
    [a, b]
  )
  console.log(`${f.padEnd(34)} ${pct < 0.01 ? 'same' : `${pct.toFixed(2)}% changed`}`)
  if (pct < 0.01) continue
  await p.setContent(`<body style="margin:0;background:#000;color:#ccc;font:600 22px system-ui">
    <div style="display:flex;gap:12px;padding:12px;align-items:flex-start">
      ${[['before', a], ['after (changes in magenta)', diff]]
        .map(([t, s]) => `<figure style="margin:0;flex:1"><figcaption style="padding:0 0 8px">${t}</figcaption>
          <img style="width:100%;outline:1px solid #333" src="${s}"></figure>`)
        .join('')}
    </div></body>`)
  await p.setViewportSize({ width: 2800, height: 400 })
  await p.locator('div').first().screenshot({ path: `${out}/${f}` })
}
await browser.close()
