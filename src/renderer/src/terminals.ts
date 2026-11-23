import type { VibeDrop } from './api'
import { Terminal, type ITheme } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebglAddon } from '@xterm/addon-webgl'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { Unicode11Addon } from '@xterm/addon-unicode11'
import type { PtySpawnOptions } from '../../shared/types'
import { attachBlob, attachPaths, attachmentsOf, takeAttachments } from './attachments'

export const termTheme: ITheme = {
  // macOS dark: the system palette on a graphite background (same as --bg-term in styles.css)
  background: '#1c1c1e',
  foreground: '#e5e5ea',
  cursor: '#0a84ff',
  cursorAccent: '#1c1c1e',
  selectionBackground: '#0a84ff4d',
  black: '#3a3a3c',
  red: '#ff453a',
  green: '#32d74b',
  yellow: '#ffd60a',
  blue: '#409cff',
  magenta: '#bf5af2',
  cyan: '#64d2ff',
  white: '#d1d1d6',
  brightBlack: '#8e8e93',
  brightRed: '#ff6961',
  brightGreen: '#5be37a',
  brightYellow: '#ffe45c',
  brightBlue: '#70b5ff',
  brightMagenta: '#da8fff',
  brightCyan: '#8be0ff',
  brightWhite: '#ffffff'
}

export const termThemeLight: ITheme = {
  background: '#fbfbfd',
  foreground: '#1d1d1f',
  cursor: '#007aff',
  cursorAccent: '#fbfbfd',
  selectionBackground: '#007aff33',
  black: '#1d1d1f',
  red: '#d70015',
  green: '#248a3d',
  yellow: '#a05a00',
  blue: '#0064d6',
  magenta: '#8944ab',
  cyan: '#0071a4',
  white: '#8e8e93',
  brightBlack: '#6e6e73',
  brightRed: '#ff3b30',
  brightGreen: '#2aa84a',
  brightYellow: '#c27c00',
  brightBlue: '#007aff',
  brightMagenta: '#af52de',
  brightCyan: '#0e94c4',
  brightWhite: '#3a3a3c'
}

let currentTheme: ITheme = termTheme
/** see-through mode: the terminal paints no background, the panel behind it does */
let clearBg = false

/** one see-through copy per palette: a new object each time made every terminal rebuild its colours */
const clearThemes = new WeakMap<ITheme, ITheme>()
const shownTheme = (): ITheme => {
  if (!clearBg) return currentTheme
  let t = clearThemes.get(currentTheme)
  if (!t) clearThemes.set(currentTheme, (t = { ...currentTheme, background: '#00000000' }))
  return t
}

/**
 * Claude Code and most CLIs paint for a dark terminal: pale greys and pure white, which all but
 * vanish on the light background. There xterm darkens any text below this contrast ratio; the dark
 * theme (and see-through, which has no background colour to measure against) keeps exact colours.
 */
const shownContrast = (): number => (currentTheme === termThemeLight && !clearBg ? 7 : 1)

function applyColors(): void {
  for (const e of entries.values()) {
    e.term.options.allowTransparency = clearBg
    e.term.options.theme = shownTheme()
    e.term.options.minimumContrastRatio = shownContrast()
  }
}

/** Switch every open terminal (and future ones) between the dark and light palettes. */
export function setTerminalTheme(light: boolean): void {
  // runs on every settings change (a slider drag): only an actual switch re-themes the terminals
  const next = light ? termThemeLight : termTheme
  if (next === currentTheme) return
  currentTheme = next
  applyColors()
}

export function setTerminalClear(clear: boolean): void {
  if (clear === clearBg) return
  clearBg = clear
  applyColors()
}

/* ---- hide Claude Code's "bypass permissions on (shift+tab to cycle)" footer line ---- */

const FOOTER_RE = /bypass permissions on/i
const footerCover = new WeakMap<Entry, HTMLDivElement>()
const footerPending = new WeakSet<Entry>()

function scheduleFooterHide(e: Entry): void {
  // a chat that isn't on screen: done when it's shown again (attachTerm)
  if (footerPending.has(e) || !terminalIsRendered(e)) return
  footerPending.add(e)
  requestAnimationFrame(() => {
    footerPending.delete(e)
    if (terminalIsRendered(e)) hideFooter(e)
  })
}

/**
 * Lay a background-coloured strip over that footer row. Only the last few rows of the screen are
 * checked, so the same words inside Claude's answers stay visible. Nothing is removed from the
 * output itself (that would break Claude's own screen redraws).
 */
function hideFooter(e: Entry): void {
  const b = e.term.buffer.active
  let row = -1
  for (let r = e.term.rows - 1; r >= Math.max(0, e.term.rows - 6); r--) {
    if (FOOTER_RE.test(b.getLine(b.viewportY + r)?.translateToString(true) ?? '')) {
      row = r
      break
    }
  }
  // the cover is a plain div over the terminal screen, placed on that row (xterm's own
  // decorations didn't show over the WebGL canvas)
  const screen = e.el.querySelector<HTMLElement>('.xterm-screen')
  let cover = footerCover.get(e)
  if (row < 0 || !screen) {
    if (cover) cover.style.display = 'none'
    return
  }
  if (!cover || cover.parentElement !== screen) {
    cover?.remove()
    cover = document.createElement('div')
    cover.className = 'term-footer-cover'
    screen.appendChild(cover)
    footerCover.set(e, cover)
  }
  const cellH = screen.clientHeight / e.term.rows
  const css = `position:absolute;left:0;right:0;top:${row * cellH}px;height:${Math.ceil(cellH)}px;z-index:20;pointer-events:none;background:${currentTheme.background ?? '#1c1c1e'};`
  // runs after every output chunk: touch the DOM only when the row actually moved
  if (cover.dataset.css !== css) {
    cover.dataset.css = css
    cover.style.cssText = css
  }
}

interface Entry {
  term: Terminal
  fit: FitAddon
  el: HTMLDivElement
  spawn: Omit<PtySpawnOptions, 'cols' | 'rows'>
  spawned: boolean
  webgl?: WebglAddon
  /** size last sent to the process, so a fit that changes nothing doesn't make Claude redraw */
  sent?: string
  resizeTimer?: ReturnType<typeof setTimeout>
}

type ActivityFn = (id: string) => void
type TitleFn = (id: string, title: string) => void
type ExitFn = (id: string, code: number) => void
type DataFn = (id: string, data: string) => void
type LinkFn = (id: string, url: string) => void

const entries = new Map<string, Entry>()
/** parked terminals still holding their WebGL renderer, oldest first */
const warm: Entry[] = []
const WARM_MAX = 1

/* Terminal font size: Ctrl + / Ctrl − / Ctrl 0 in any terminal, shared by all, remembered. */
const FONT_KEY = 'tinjs.termFontSize'
const FONT_DEFAULT = 12
const FONT_MIN = 9
const FONT_MAX = 22
let fontSize = ((): number => {
  try {
    const n = Number(localStorage.getItem(FONT_KEY))
    return n >= FONT_MIN && n <= FONT_MAX ? n : FONT_DEFAULT
  } catch {
    return FONT_DEFAULT
  }
})()

export function setTermFontSize(size: number): void {
  const next = Math.max(FONT_MIN, Math.min(FONT_MAX, Math.round(size)))
  if (next === fontSize) return
  fontSize = next
  try {
    localStorage.setItem(FONT_KEY, String(next))
  } catch {
    /* storage unavailable */
  }
  for (const e of entries.values()) {
    e.term.options.fontSize = next
    doFit(e)
  }
}

type InputFn = (id: string, data: string) => boolean
const handlers: { activity?: ActivityFn; title?: TitleFn; exit?: ExitFn; data?: DataFn; link?: LinkFn; input?: InputFn } = {}

let parking: HTMLDivElement | null = null
function parkingLot(): HTMLDivElement {
  if (!parking) {
    parking = document.createElement('div')
    parking.style.cssText = 'position:fixed;left:-10000px;top:0;width:800px;height:400px;visibility:hidden;'
    document.body.appendChild(parking)
  }
  return parking
}

let wired = false
function wire(): void {
  if (wired) return
  wired = true
  window.api.pty.onData((id, data) => {
    const e = entries.get(id)
    if (!e) return
    e.term.write(data, () => {
      if (entries.get(id) !== e) return
      scheduleFooterHide(e)
      handlers.activity?.(id)
    })
    handlers.data?.(id, data)
  })
  window.api.pty.onExit((id, code) => {
    const e = entries.get(id)
    // -1 = found dead on a keystroke; the chat restarts right away, so no note
    if (e && code !== -1) e.term.write(`\r\n\x1b[90m[process exited with code ${code}]\x1b[0m\r\n`)
    handlers.exit?.(id, code)
  })
}

export function onTerminalEvents(h: { activity?: ActivityFn; title?: TitleFn; exit?: ExitFn; data?: DataFn; link?: LinkFn; input?: InputFn }): void {
  Object.assign(handlers, h)
}

/* ---- image paths in terminal output: hover preview + click to enlarge ---- */

const PATH_RE = /[A-Za-z]:[\\/][^\s"'<>|*?:`]*/g
const IMAGE_EXT_RE = /\.(?:png|jpe?g|gif|webp|bmp|svg|avif)$/i

/** Show a path in File Explorer: a folder opens, a file is selected in its folder.
 *  A path cut short (e.g. at a space) falls back to the nearest folder that exists. */
async function revealPath(path: string): Promise<void> {
  let p = path.replace(/\//g, '\\')
  for (;;) {
    const info = (await window.api.app.fileInfo([p]).catch(() => []))[0]
    if (info?.dir) return void window.api.fs.openPath(p)
    if (info && info.size >= 0) return void window.api.fs.reveal(p)
    const cut = p.lastIndexOf('\\')
    if (cut < 3) return
    p = p.slice(0, cut)
  }
}
type CellPos = { x: number; y: number }

/** The whole logical line around buffer row `y` (1-based), joining rows soft-wrapped by the
 *  terminal, with the 1-based cell of every character so a match maps back to the screen. */
function logicalLine(term: Terminal, y: number): { text: string; pos: CellPos[]; first: number; last: number } {
  const buf = term.buffer.active
  let row = y - 1
  while (row > 0 && buf.getLine(row)?.isWrapped) row--
  const first = row + 1
  let text = ''
  const pos: CellPos[] = []
  const cell = buf.getNullCell()
  for (let line = buf.getLine(row); line; line = buf.getLine(++row)) {
    if (row + 1 > first && !line.isWrapped) break
    for (let col = 0; col < line.length; col++) {
      if (!line.getCell(col, cell) || cell.getWidth() === 0) continue
      const chars = cell.getChars() || ' '
      for (const ch of chars) {
        text += ch
        for (let i = 0; i < ch.length; i++) pos.push({ x: col + 1, y: row + 1 })
      }
    }
  }
  const trimmed = text.replace(/\s+$/, '')
  // `row` stopped on the first row after the line (0-based), i.e. the line's last row 1-based
  return { text: trimmed, pos: pos.slice(0, trimmed.length), first, last: row }
}

type Line = ReturnType<typeof logicalLine>

/**
 * Claude Code wraps long lines itself (a newline plus indentation, not a terminal soft wrap), which
 * cut paths in two. `b` goes on from `a` when `b` is indented text and either `a` runs to the right
 * edge ('char': cut mid-word) or `b`'s first word would not have fit after `a` ('word': wrapped at
 * a space, which Claude Code drops, e.g. "...\Roaming\TINJS" / "Studio\zalo\...").
 */
function continues(term: Terminal, a: Line, b: Line): 'char' | 'word' | null {
  const end = a.pos[a.pos.length - 1]
  const word = /^\s+(\S+)/.exec(b.text)?.[1]
  if (!end || !word) return null
  if (end.x >= term.cols - 2) return 'char'
  return end.x + 1 + word.length >= term.cols - 2 ? 'word' : null
}

/**
 * The logical line through row `y` joined with the lines Claude Code hard-wrapped it into (a few
 * each way); `joins` are the text offsets where a row cut mid-word was appended. A row wrapped at
 * a space is appended after a space, so a path with spaces is still checked on disk whole.
 */
function wrappedBlock(term: Terminal, y: number): { text: string; pos: CellPos[]; joins: number[] } {
  const rows = term.buffer.active.length
  const lines = [logicalLine(term, y)]
  const kinds: ('char' | 'word')[] = []
  for (let i = 0; i < 4 && lines[0].first > 1; i++) {
    const prev = logicalLine(term, lines[0].first - 1)
    const kind = continues(term, prev, lines[0])
    if (!kind) break
    lines.unshift(prev)
    kinds.unshift(kind)
  }
  for (let i = 0; i < 4 && lines[lines.length - 1].last < rows; i++) {
    const next = logicalLine(term, lines[lines.length - 1].last + 1)
    const kind = continues(term, lines[lines.length - 1], next)
    if (!kind) break
    lines.push(next)
    kinds.push(kind)
  }
  let text = lines[0].text
  const pos = lines[0].pos.slice()
  const joins: number[] = []
  lines.slice(1).forEach((l, i) => {
    const indent = l.text.length - l.text.trimStart().length
    if (kinds[i] === 'word') {
      // the dropped space sat right after the previous row's last character
      const last = pos[pos.length - 1]
      text += ' '
      pos.push({ x: last.x + 1, y: last.y })
    } else joins.push(text.length)
    text += l.text.slice(indent)
    pos.push(...l.pos.slice(indent))
  })
  return { text, pos, joins }
}

const trimPathEnd = (s: string): string => s.replace(/[.,;)\]]+$/, '')
/** words that may continue a path cut at a space, e.g. "...\nsis\VIBE CODE_0.2.0_x64-setup.exe" */
const PATH_TAIL_RE = /^(?: [^\s"'<>|*?:`]+){1,6}/

/** Windows paths on the logical line through row `y`. A path with spaces is taken as long as
 *  the longest version of it that exists on disk; the plain regex match otherwise. A path running
 *  over a line Claude Code wrapped stays whole when it exists (or its folder does). */
async function findPathLinks(term: Terminal, y: number): Promise<{ path: string; start: CellPos; end: CellPos }[]> {
  const { text, pos, joins } = wrappedBlock(term, y)
  const out: { path: string; start: CellPos; end: CellPos }[] = []
  for (const m of text.matchAll(PATH_RE)) {
    const at = m.index
    let match = m[0]
    const cut = joins.find((j) => j > at && j < at + match.length)
    if (cut !== undefined) {
      // two lines that only look like one: keep the part before the wrap
      const whole = trimPathEnd(match)
      const parent = whole.replace(/[\\/][^\\/]*$/, '')
      const [w, p] = await window.api.app.fileInfo([whole, parent]).catch(() => [])
      if (!(w && (w.dir || w.size >= 0)) && !(p?.dir && parent.length > 3)) match = match.slice(0, cut - at)
    }
    let path = trimPathEnd(match)
    const tail = PATH_TAIL_RE.exec(text.slice(at + match.length))?.[0]
    if (tail) {
      const candidates: string[] = []
      let acc = match
      for (const word of tail.split(' ').slice(1)) candidates.push(trimPathEnd((acc += ' ' + word)))
      const infos = await window.api.app.fileInfo(candidates).catch(() => [])
      for (let i = infos.length - 1; i >= 0; i--) {
        if (infos[i].dir || infos[i].size >= 0) {
          path = candidates[i]
          break
        }
      }
    }
    if (!path) continue
    const start = pos[at]
    const end = pos[at + path.length - 1]
    // only links that touch the row being asked about
    if (start && end && start.y <= y && end.y >= y) out.push({ path, start, end })
  }
  return out
}

const imageCache = new Map<string, Promise<string | undefined>>()
let hoverEl: HTMLImageElement | null = null
let hoverToken = 0

/** previews are data: URLs (up to a few MB each): keep only the last few hovered */
const IMAGE_CACHE_MAX = 24

function loadImage(path: string): Promise<string | undefined> {
  let p = imageCache.get(path)
  if (p) {
    // most recently used goes to the end
    imageCache.delete(path)
    imageCache.set(path, p)
  } else {
    if (imageCache.size >= IMAGE_CACHE_MAX) imageCache.delete(imageCache.keys().next().value!)
    p = window.api.app
      .fileInfo([path])
      .then((r) => r[0]?.preview)
      .catch(() => undefined)
    // a missing file may appear later: don't remember failures
    p.then((src) => !src && imageCache.delete(path))
    imageCache.set(path, p)
  }
  return p
}

async function showImageHover(path: string, e: MouseEvent): Promise<void> {
  const token = ++hoverToken
  const src = await loadImage(path)
  if (!src || token !== hoverToken) return
  if (!hoverEl) {
    hoverEl = document.createElement('img')
    hoverEl.className = 'term-image-hover'
    document.body.appendChild(hoverEl)
  }
  hoverEl.src = src
  const below = e.clientY < window.innerHeight / 2
  hoverEl.style.left = `${Math.min(e.clientX + 12, window.innerWidth - 336)}px`
  hoverEl.style.top = below ? `${e.clientY + 18}px` : ''
  hoverEl.style.bottom = below ? '' : `${window.innerHeight - e.clientY + 12}px`
  hoverEl.style.display = 'block'
}

function hideImageHover(): void {
  hoverToken++
  if (hoverEl) hoverEl.style.display = 'none'
}

async function showImageLightbox(path: string): Promise<void> {
  hideImageHover()
  const src = await loadImage(path)
  if (!src) return
  const box = document.createElement('div')
  box.className = 'image-lightbox'
  const img = document.createElement('img')
  img.src = src
  box.appendChild(img)
  const close = (): void => {
    box.remove()
    window.removeEventListener('keydown', onKey, true)
  }
  const onKey = (ev: KeyboardEvent): void => {
    if (ev.key !== 'Escape') return
    ev.preventDefault()
    ev.stopPropagation()
    close()
  }
  box.addEventListener('click', close)
  window.addEventListener('keydown', onKey, true)
  document.body.appendChild(box)
}

export function createTerm(spawn: Omit<PtySpawnOptions, 'cols' | 'rows'>): void {
  wire()
  if (entries.has(spawn.id)) return
  const openLink = (url: string): void => {
    if (!/^https?:\/\//i.test(url)) return
    if (handlers.link) handlers.link(spawn.id, url)
    else window.api.app.openExternal(url)
  }
  const term = new Terminal({
    // OSC 8 hyperlinks (Claude Code underlines URLs this way): same route as plain-text links,
    // instead of xterm's default confirm() + window.open, which ended up in the external browser
    linkHandler: { activate: (_e, url) => openLink(url), allowNonHttpProtocols: false },
    fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
    fontSize,
    lineHeight: 1.1,
    letterSpacing: 0,
    cursorBlink: true,
    cursorStyle: 'bar',
    cursorWidth: 2,
    allowProposedApi: true,
    // Claude Code keeps the full transcript itself; 5k lines per chat keeps 10+ chats light
    scrollback: 5000,
    smoothScrollDuration: 125,
    theme: shownTheme(),
    minimumContrastRatio: shownContrast(),
    // only see-through mode needs a transparent canvas: with it on, xterm rasterises every glyph
    // without a background behind it, which draws text thinner and paler (worst on the light theme)
    allowTransparency: clearBg,
    macOptionIsMeta: true
  })
  const fit = new FitAddon()
  term.loadAddon(fit)
  // clicked links go to the app (a chat's own Web tab); external browser only as a fallback
  term.loadAddon(new WebLinksAddon((_e, url) => openLink(url)))
  // Windows paths: images get a hover thumbnail + click to enlarge, anything else opens in File Explorer
  term.registerLinkProvider({
    provideLinks(y, cb) {
      void findPathLinks(term, y).then((found) => {
        const links = found.map(({ path, start, end }) => {
          const image = IMAGE_EXT_RE.test(path)
          return {
            range: { start, end },
            text: path,
            activate: () => void (image ? showImageLightbox(path) : revealPath(path)),
            hover: (e: MouseEvent) => void (image && showImageHover(path, e)),
            leave: hideImageHover
          }
        })
        cb(links.length ? links : undefined)
      })
    }
  })
  const uni = new Unicode11Addon()
  term.loadAddon(uni)
  term.unicode.activeVersion = '11'

  term.attachCustomKeyEventHandler((ev) => {
    if (ev.type !== 'keydown') return true
    const ctrl = ev.ctrlKey || ev.metaKey
    // Ctrl+C with a selection, or Ctrl+Shift+C (Windows Terminal / VS Code): copy
    if (ctrl && ev.key.toLowerCase() === 'c' && (ev.shiftKey || term.hasSelection())) {
      if (!term.hasSelection()) return false
      navigator.clipboard.writeText(term.getSelection())
      term.clearSelection()
      return false
    }
    // Ctrl+A selects everything in the terminal (then Ctrl+C copies). Home still jumps to line start.
    if (ctrl && !ev.shiftKey && !ev.altKey && ev.key.toLowerCase() === 'a') {
      term.selectAll()
      return false
    }
    // Ctrl + / Ctrl − / Ctrl 0: terminal font size
    if (ctrl && !ev.altKey && (ev.key === '=' || ev.key === '+')) {
      setTermFontSize(fontSize + 1)
      return false
    }
    if (ctrl && !ev.altKey && ev.key === '-') {
      setTermFontSize(fontSize - 1)
      return false
    }
    if (ctrl && !ev.altKey && ev.key === '0') {
      setTermFontSize(FONT_DEFAULT)
      return false
    }
    // Let the browser fire a native paste event, which xterm turns into bracketed paste.
    if (ctrl && ev.key.toLowerCase() === 'v') return false
    // Shift+Enter → newline inside Claude's prompt (ESC + CR, same as Meta+Enter)
    if (ev.key === 'Enter' && ev.shiftKey && !ctrl) {
      window.api.pty.write(spawn.id, '\x1b\r')
      return false
    }
    return true
  })

  term.onData((raw) => {
    /*
     * Focus in/out reports (ESC[I / ESC[O) never reach the process. xterm sends ESC[O whenever a
     * chat's terminal loses focus, e.g. when you switch to another chat and this one is parked, and
     * through ConPTY that can arrive as a bare Esc, which interrupts whatever Claude was doing.
     */
    // eslint-disable-next-line no-control-regex
    const d = raw.replace(/\x1b\[[IO]/g, '')
    if (!d) return
    // the handler may take the keystroke instead (e.g. to restart an ended chat)
    if (handlers.input?.(spawn.id, d)) return
    // Enter with files in the tray: their paths go into the prompt first, then the Enter.
    // Not while Claude shows a numbered menu (permission prompt…) — there Enter picks an option.
    if (d === '\r' && spawn.kind === 'claude' && attachmentsOf(spawn.id).length && !menuOnScreen(term)) {
      const paths = takeAttachments(spawn.id).map((a) => quotePath(a.path))
      window.api.pty.write(spawn.id, ' ' + paths.join(' ') + ' ')
      // a separate Enter, so Claude doesn't read it as part of the typed-in block
      setTimeout(() => window.api.pty.write(spawn.id, '\r'), 120)
      return
    }
    window.api.pty.write(spawn.id, d)
  })
  term.onTitleChange((t) => handlers.title?.(spawn.id, t))

  const el = document.createElement('div')
  el.className = 'xterm-host'
  parkingLot().appendChild(el)
  term.open(el)

  // Ctrl+V with a picture (or files copied in Explorer) on the clipboard: into the attachment tray.
  el.addEventListener(
    'paste',
    (ev) => {
      const files = [...(ev.clipboardData?.files ?? [])]
      if (!files.length) return
      ev.preventDefault()
      ev.stopPropagation()
      void takeFiles(spawn.id, spawn.kind, files)
    },
    true
  )
  // Drop files from Explorer (pictures or anything else) onto the chat → the attachment tray.
  el.addEventListener('vibe-drop', (ev) => {
    const d = (ev as CustomEvent<VibeDrop>).detail
    if (d.kind === 'enter') el.classList.add('drop-hover')
    else if (d.kind === 'leave') el.classList.remove('drop-hover')
    else {
      takePaths(spawn.id, spawn.kind, d.paths)
      term.focus()
    }
  })
  // Right-click: copy the selection, or paste when nothing is selected.
  el.addEventListener('contextmenu', (ev) => {
    ev.preventDefault()
    if (term.hasSelection()) {
      navigator.clipboard.writeText(term.getSelection())
      term.clearSelection()
    } else pasteInto(spawn.id)
  })

  const entry: Entry = { term, fit, el, spawn, spawned: false }
  entries.set(spawn.id, entry)
  // keep the footer cover on the right row when scrolling or resizing
  term.onScroll(() => scheduleFooterHide(entry))
  term.onResize(() => scheduleFooterHide(entry))
}

function doFit(e: Entry): void {
  const parent = e.el.parentElement
  if (!parent || parent === parking || parent.clientWidth < 20 || parent.clientHeight < 20) return
  try {
    e.fit.fit()
  } catch {
    return
  }
  if (e.spawned) sendSize(e)
}

/**
 * Every resize makes Claude Code repaint its whole screen (and ConPTY reflow it), so while a panel
 * is being dragged or animated, only the final size goes to the process.
 */
function sendSize(e: Entry): void {
  clearTimeout(e.resizeTimer)
  e.resizeTimer = setTimeout(() => {
    const size = `${e.term.cols}x${e.term.rows}`
    if (size === e.sent) return
    e.sent = size
    window.api.pty.resize(e.spawn.id, e.term.cols, e.term.rows)
  }, 120)
}

/**
 * Chromium allows only a handful of live WebGL contexts per page (the Web tab uses some too).
 * When one runs out, the oldest terminal's canvas is lost and it freezes or goes blank mid-output,
 * so a terminal holds its context only while it is on screen.
 */
function terminalIsRendered(e: Entry): boolean {
  return e.el.isConnected && e.el.parentElement !== parking && !e.el.closest('[inert]')
}

function loadWebgl(e: Entry): void {
  if (e.webgl || !terminalIsRendered(e)) return
  try {
    const gl = new WebglAddon()
    gl.onContextLoss(() => {
      gl.dispose()
      if (e.webgl === gl) e.webgl = undefined
      // still showing: get a fresh context instead of staying on the slow fallback renderer
      setTimeout(() => {
        loadWebgl(e)
      }, 500)
    })
    e.term.loadAddon(gl)
    e.webgl = gl
  } catch {
    /* falls back to DOM renderer */
  }
}

export function attachTerm(id: string, container: HTMLElement, focus = true): void {
  const e = entries.get(id)
  if (!e) return
  const w = warm.indexOf(e)
  if (w >= 0) warm.splice(w, 1)
  container.appendChild(e.el)
  // on screen: its output streams at full rate again
  window.api.pty.visible(id, true)
  scheduleFooterHide(e)
  const fresh = !e.spawned
  // An already-running chat gets its GPU renderer right away (its screen is full). A new one starts
  // empty, so the WebGL setup (GPU context, shaders, glyph atlas: the slowest part of opening a
  // terminal) waits until the process is launched and the chat's enter animation has had its
  // frames — Claude takes far longer than that to print anything.
  if (!fresh) loadWebgl(e)
  // setTimeout, not requestAnimationFrame: rAF is paused while the window is hidden or
  // minimized, which would leave a new chat stuck at "Starting…" with no process.
  setTimeout(() => {
    doFit(e)
    if (!e.spawned) {
      e.spawned = true
      e.sent = `${e.term.cols}x${e.term.rows}`
      window.api.pty.spawn({ ...e.spawn, cols: e.term.cols, rows: e.term.rows })
    }
    if (focus) e.term.focus()
    if (fresh) whenIdle(() => e.el.parentElement && e.el.parentElement !== parking && loadWebgl(e), 400)
  }, 0)
}

/** Run once the page has a free moment (at most `timeout` ms later). */
function whenIdle(fn: () => void, timeout: number): void {
  if (typeof requestIdleCallback === 'function') requestIdleCallback(fn, { timeout })
  else setTimeout(fn, timeout)
}

export function detachTerm(id: string): void {
  const e = entries.get(id)
  if (!e || e.el.parentElement === parking) return
  parkingLot().appendChild(e.el)
  // off screen: the backend batches its output into fewer, bigger pieces
  window.api.pty.visible(id, false)
  // the chat you just left keeps its GPU renderer, so switching back and forth doesn't rebuild one
  // each time; older parked ones give theirs up (contexts are limited, see loadWebgl)
  if (!e.webgl) return
  warm.push(e)
  while (warm.length > WARM_MAX) {
    const o = warm.shift()!
    if (o.el.parentElement === parking) {
      o.webgl?.dispose()
      o.webgl = undefined
    }
  }
}

export function fitTerm(id: string): void {
  const e = entries.get(id)
  if (e) doFit(e)
}

export function syncTermRenderer(id: string): void {
  const e = entries.get(id)
  if (!e) return
  if (!terminalIsRendered(e)) {
    e.webgl?.dispose()
    e.webgl = undefined
    return
  }
  scheduleFooterHide(e)
  whenIdle(() => loadWebgl(e), 180)
}

export function focusTerm(id: string): void {
  entries.get(id)?.term.focus()
}

const quotePath = (p: string): string => (/\s/.test(p) ? `"${p}"` : p)

/**
 * Files from a drop / paste. A chat puts them in its attachment tray (clipboard pictures saved to
 * temp first); a plain shell just gets the paths typed in.
 */
async function takeFiles(id: string, kind: PtySpawnOptions['kind'], files: File[]): Promise<void> {
  // files copied in Explorer: the page only sees their bytes, the clipboard knows their paths
  const copied: string[] = await window.api.app.clipboardFiles().catch(() => [])
  if (copied.length) return takePaths(id, kind, copied)
  for (const f of files) if (f.type.startsWith('image/') && kind === 'claude') await attachBlob(id, f)
}

function takePaths(id: string, kind: PtySpawnOptions['kind'], paths: string[]): void {
  if (kind === 'claude') attachPaths(id, paths)
  else if (paths.length) writeToTerm(id, paths.map(quotePath).join(' ') + ' ')
}

/** Files from outside the page (a screen capture): into the chat's tray, or typed into a shell. */
export function giveFiles(id: string, paths: string[]): void {
  const e = entries.get(id)
  if (!e) return
  takePaths(id, e.spawn.kind, paths)
  focusTerm(id)
}

/** Claude is showing a numbered choice (permission prompt, plan approval…): "❯ 1. Yes" near the bottom. */
function menuOnScreen(term: Terminal): boolean {
  const b = term.buffer.active
  const last = Math.min(term.rows - 1, b.cursorY + 5)
  for (let r = last; r >= Math.max(0, b.cursorY - 18); r--) {
    if (/^\s*(?:[❯›]\s*\d+\.|enter to (?:confirm|select)\b)/i.test(b.getLine(b.baseY + r)?.translateToString(true) ?? '')) return true
  }
  return false
}

export function chatNeedsInput(id: string): boolean {
  const entry = entries.get(id)
  return entry ? menuOnScreen(entry.term) : false
}

/** Paste button / right-click: an image if the clipboard holds one, else text. */
export async function pasteInto(id: string): Promise<void> {
  try {
    for (const item of await navigator.clipboard.read()) {
      const type = item.types.find((t) => t.startsWith('image/'))
      if (type) {
        const blob = await item.getType(type)
        if (entries.get(id)?.spawn.kind === 'claude') await attachBlob(id, blob)
        else {
          const ext = (blob.type.split('/')[1] || 'png').replace('jpeg', 'jpg')
          writeToTerm(id, quotePath(await window.api.app.saveImage(new Uint8Array(await blob.arrayBuffer()), ext)) + ' ')
        }
        return focusTerm(id)
      }
    }
  } catch {
    /* clipboard.read can refuse; fall back to text */
  }
  const text = await navigator.clipboard.readText().catch(() => '')
  const e = entries.get(id)
  if (text && e) e.term.paste(text)
  focusTerm(id)
}

/** Copy button: the selection, or the whole visible screen when nothing is selected. */
export function copyFrom(id: string): void {
  const e = entries.get(id)
  if (!e) return
  const text = e.term.hasSelection() ? e.term.getSelection() : readScreen(id)
  navigator.clipboard.writeText(text)
  e.term.clearSelection()
  focusTerm(id)
}

export function writeToTerm(id: string, text: string): void {
  window.api.pty.write(id, text)
}

function agentPromptOnScreen(term: Terminal): { prefix: string; text: string; row: number } | undefined {
  const buffer = term.buffer.active
  const shellPrompt = /^\s*(?:PS\s+[^>]+|[A-Z]:\\[^>]*)>|^\s*[\w.-]+@[\w.-]+:.*[$#]\s*$/i
  for (let row = term.rows - 1; row >= 0; row--) {
    const line = buffer.getLine(buffer.baseY + row)?.translateToString(true) ?? ''
    if (shellPrompt.test(line)) return
    const match = line.match(/^(\s*[❯›>]\s*)(.*)$/)
    if (match) return { prefix: match[1], text: match[2], row }
  }
}

function waitForDraftEcho(term: Terminal, text: string): Promise<void> {
  // The normal short prompt can be submitted as soon as the agent has redrawn it.
  // Keep the previous delay for wrapped or multiline prompts that can't be matched reliably.
  const expected = text.trim()
  return new Promise((resolve) => {
    let subscription: { dispose(): void } | undefined
    const finish = (): void => {
      clearTimeout(timeout)
      subscription?.dispose()
      resolve()
    }
    const timeout = setTimeout(finish, 150)
    if (expected.length >= term.cols - 6 || /[\n\t]/.test(expected)) return
    if (agentPromptOnScreen(term)?.text.trim() === expected) finish()
    else subscription = term.onWriteParsed(() => {
      if (agentPromptOnScreen(term)?.text.trim() === expected) finish()
    })
  })
}

/** Use the same paste and Enter path as manual input, including pending attachments. */
export async function submitChatDraft(id: string, text: string): Promise<void> {
  const entry = entries.get(id)
  if (!entry || entry.spawn.kind !== 'claude') throw new Error('Cuộc trò chuyện chưa sẵn sàng.')
  if (menuOnScreen(entry.term)) throw new Error('AI đang chờ bạn chọn trong cuộc trò chuyện phía trên.')
  if (/enter to confirm|esc to cancel|escape to cancel/i.test(readScreenTail(id, 12))) {
    throw new Error('Hãy xử lý yêu cầu xác nhận phía trên trước khi gửi.')
  }
  // On Windows the shell can stay open if the agent fails to launch or exits.
  // Only send when the visible input is an agent prompt, never a shell prompt.
  const { term } = entry
  const buffer = term.buffer.active
  const visible = agentPromptOnScreen(term)
  if (!visible) throw new Error('AI chưa sẵn sàng. Mở chế độ gốc để kiểm tra.')
  // Claude draws a suggestion to the right of the caret even on an empty prompt.
  const suggestion = /^Try\s+["“']/i.test(visible.text) && buffer.cursorY === visible.row && buffer.cursorX <= visible.prefix.length
  if (visible.text.trim() && !suggestion) {
    throw new Error('Ô nhập ở chế độ gốc còn bản nháp. Mở chế độ gốc để xử lý trước.')
  }
  const prompt = text.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f\x80-\x9f]/g, '').replace(/\r\n?/g, '\n').trim()
  if (!prompt) throw new Error('Hãy nhập nội dung cần gửi.')
  const paste = entry.term.modes.bracketedPasteMode ? prompt : prompt.replace(/\s*[\n\t]\s*/g, ' ')
  entry.term.paste(paste)
  await waitForDraftEcho(term, paste)
  if (entries.get(id) !== entry || menuOnScreen(term) || !agentPromptOnScreen(term)) {
    throw new Error('Chat đã đổi trạng thái. Mở chế độ gốc trước khi gửi lại.')
  }
  entry.term.input('\r', true)
}

export function restartTerm(id: string, spawn?: Partial<Omit<PtySpawnOptions, 'cols' | 'rows'>>): void {
  const e = entries.get(id)
  if (!e) return
  e.spawn = { ...e.spawn, ...spawn }
  e.term.reset()
  e.sent = `${e.term.cols}x${e.term.rows}`
  window.api.pty.spawn({ ...e.spawn, cols: e.term.cols, rows: e.term.rows })
}

export function disposeTerm(id: string): void {
  const e = entries.get(id)
  if (!e) return
  window.api.pty.kill(id)
  clearTimeout(e.resizeTimer)
  const w = warm.indexOf(e)
  if (w >= 0) warm.splice(w, 1)
  e.webgl?.dispose()
  e.term.dispose()
  e.el.remove()
  entries.delete(id)
}

/** The last `n` rows of a terminal's screen as text (cheap: status checks run often). */
export function readScreenTail(id: string, n: number): string {
  const e = entries.get(id)
  if (!e) return ''
  const b = e.term.buffer.active
  const lines: string[] = []
  for (let i = Math.max(0, e.term.rows - n); i < e.term.rows; i++) lines.push(b.getLine(b.viewportY + i)?.translateToString(true) ?? '')
  return lines.join('\n')
}

/** Plain-text dump of a terminal's visible screen (debugging / scripted checks). */
export function readScreen(id: string): string {
  const e = entries.get(id)
  if (!e) return ''
  const b = e.term.buffer.active
  const lines: string[] = []
  for (let i = 0; i < e.term.rows; i++) lines.push(b.getLine(b.viewportY + i)?.translateToString(true) ?? '')
  return lines.join('\n').replace(/\n+$/, '')
}
