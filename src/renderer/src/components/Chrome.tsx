import { getCurrentWindow } from '@tauri-apps/api/window'
import { useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import { SlidePill } from './motion'
import {
  ChevronDown,
  CalendarDays,
  CircleCheck,
  CircleAlert,
  Files,
  FolderOpen,
  GitCompare,
  Loader2,
  MessagesSquare,
  PanelLeft,
  Search,
  Clock,
  ArrowDownToLine,
  ChevronUp,
  Minus,
  Square,
  Copy,
  X,
  Link2,
  Check,
  Globe,
  Moon,
  Sun,
  SquareDashedPlus
} from 'lucide-react'
import { basename, useStore, type SidebarView } from '../store'
import { defaultAgent, installedAgents, openBrowser, openFile, openFolder, openSession, setDefaultAgent, toggleTheme } from '../actions'
import { agentDef } from '../../../shared/agents'
import { FileIcon, Logo } from './icons'
import type { UsageLimit } from '../../../shared/types'
import { revealIn } from '../scroll'
import { giveFiles } from '../terminals'
import { COMMANDS, type Command } from '../shortcuts'

/* ================================ Title bar ================================ */

/** Empty parts of the title bar move the window (the drop-down stays pinned to the top edge). */
const NO_DRAG = 'button, input, a, kbd, .menu, .search-box, .folder-switch, .win-controls'
// A double-click is handled here (e.detail === 2), not in onDoubleClick: once the first click
// has started a native drag, Windows maximizes on the second click by itself, and a separate
// dblclick handler then toggled it straight back — the window flickered and the
// maximize/restore button showed the wrong state.
function dragWindow(e: React.MouseEvent): void {
  if (e.button !== 0 || (e.target as HTMLElement).closest(NO_DRAG)) return
  if (useStore.getState().win?.mode === 'dropdown') return
  if (e.detail === 2) window.api.win.toggleMaximize()
  else void getCurrentWindow().startDragging()
}

export function TitleBar(): React.JSX.Element {
  const root = useStore((s) => s.root)
  const recent = useStore((s) => s.recent)
  const [menu, setMenu] = useState(false)

  useEffect(() => {
    if (!menu) return
    const close = (): void => setMenu(false)
    window.addEventListener('mousedown', close)
    return () => window.removeEventListener('mousedown', close)
  }, [menu])

  return (
    <div className={window.api.platform === 'darwin' ? 'titlebar mac' : 'titlebar'} onMouseDown={dragWindow}>
      <div className="titlebar-left">
        <Logo size={18} />
        <span className="app-name">VIBE <b>CODE</b></span>
        {root && (
          <div className="folder-switch" onMouseDown={(e) => e.stopPropagation()}>
            <button className="folder-btn" onClick={() => setMenu(!menu)}>
              <FolderOpen size={13} />
              {basename(root)}
              <ChevronDown size={12} />
            </button>
            {menu && (
              <div className="menu">
                <button
                  className="menu-item"
                  onClick={() => {
                    setMenu(false)
                    openFolder()
                  }}
                >
                  <FolderOpen size={14} /> Mở thư mục…
                </button>
                {recent.length > 1 && <div className="menu-sep">Gần đây</div>}
                {recent
                  .filter((r) => r !== root)
                  .map((r) => (
                    <button
                      key={r}
                      className="menu-item"
                      onClick={() => {
                        setMenu(false)
                        openFolder(r)
                      }}
                    >
                      <Clock size={14} />
                      <span className="menu-item-main">{basename(r)}</span>
                      <span className="menu-item-sub">{r}</span>
                    </button>
                  ))}
              </div>
            )}
          </div>
        )}
      </div>
      {root && (
        <button className="search-box" onClick={() => useStore.setState({ quickOpen: true })}>
          <Search size={13} />
          <span>Tìm tệp trong {basename(root)}</span>
          <kbd>Ctrl+P</kbd>
        </button>
      )}
      <div className="titlebar-right">
        <button className="icon-btn" title="Ẩn/hiện thanh bên (Ctrl+B)" onClick={() => useStore.setState((s) => ({ sidebarOpen: !s.sidebarOpen }))}>
          <PanelLeft size={15} />
        </button>
        <button className="icon-btn" title="Mở trình duyệt" onClick={() => openBrowser()}>
          <Globe size={15} />
        </button>
        <CaptureButton />
        <ThemeToggle />
      </div>
      <WindowControls />
    </div>
  )
}

/** Screen capture: pick a region, mark it up, and it lands in the active chat (and the clipboard). */
function CaptureButton(): React.JSX.Element | null {
  useEffect(
    () =>
      window.api.app.onCaptured((path) => {
        const id = useStore.getState().activeChatId
        if (id) giveFiles(id, [path])
      }),
    []
  )
  if (window.api.platform !== 'win32') return null
  return (
    <button className="icon-btn" title="Chụp phần phía sau VIBE CODE: chọn vùng, vẽ chú thích, gửi vào chat (Ctrl+Alt+Z chụp cả màn hình, gồm cả VIBE CODE)" onClick={() => void window.api.app.capture(true)}>
      <SquareDashedPlus size={15} />
    </button>
  )
}

/** Light ↔ dark. */
function ThemeToggle(): React.JSX.Element {
  const light = useStore((s) => s.themeResolved === 'light')
  return (
    <button className="icon-btn" title={light ? 'Chuyển sang giao diện tối' : 'Chuyển sang giao diện sáng'} onClick={() => toggleTheme()}>
      {light ? <Moon size={15} /> : <Sun size={15} />}
    </button>
  )
}

function WindowControls(): React.JSX.Element | null {
  const win = useStore((s) => s.win)
  if (!win) return null
  // macOS draws its own traffic lights (top-left) — ours would be a second set
  if (window.api.platform === 'darwin' && win.mode !== 'dropdown') return null
  if (win.mode === 'dropdown') {
    return (
      <div className="win-controls">
        <button className="win-btn" title={`Ẩn (${win.hotkey})`} onClick={() => window.api.win.hide()}>
          <ChevronUp size={16} />
        </button>
      </div>
    )
  }
  return (
    <div className="win-controls">
      <button className="win-btn" title="Thu nhỏ" onClick={() => window.api.win.minimize()}>
        <Minus size={15} />
      </button>
      <button className="win-btn" title={win.maximized ? 'Khôi phục' : 'Phóng to'} onClick={() => window.api.win.toggleMaximize()}>
        {win.maximized ? <Copy size={12} style={{ transform: 'scaleX(-1)' }} /> : <Square size={12} />}
      </button>
      <button className="win-btn close" title="Đóng" onClick={() => window.api.win.close()}>
        <X size={16} />
      </button>
    </div>
  )
}

/* ================================ Activity bar ================================ */

export function ActivityBar(): React.JSX.Element {
  const view = useStore((s) => s.sidebarView)
  const open = useStore((s) => s.sidebarOpen)
  const attention = useStore((s) => s.chats.filter((c) => c.attention).length)
  const working = useStore((s) => s.chats.some((c) => c.status === 'working'))
  const changes = useStore((s) => s.git.files.length)

  const item = (v: SidebarView, icon: React.ReactNode, title: string, badge?: React.ReactNode): React.JSX.Element => (
    <button
      className={`activity-item${view === v && open ? ' active' : ''}`}
      title={title}
      onClick={() => useStore.setState((s) => (s.sidebarView === v && s.sidebarOpen ? { sidebarOpen: false } : { sidebarView: v, sidebarOpen: true }))}
    >
      {icon}
      {badge}
    </button>
  )

  return (
    <div className="activity-bar">
      <SlidePill />
      {item(
        'chats',
        <MessagesSquare size={20} strokeWidth={1.7} />,
        'Chats',
        attention > 0 ? <span className="badge accent">{attention}</span> : working ? <span className="badge-pulse" /> : null
      )}
      {item('explorer', <Files size={20} strokeWidth={1.7} />, 'Explorer (Ctrl+Shift+E)')}
      {item('changes', <GitCompare size={20} strokeWidth={1.7} />, 'Changes (Ctrl+Shift+G)', changes > 0 ? <span className="badge">{changes > 99 ? '99+' : changes}</span> : null)}
    </div>
  )
}

/* ================================ Status bar ================================ */

/** "1:00 PM" today, "Fri 3:00 AM" later in the week. */
function resetLabel(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso)
  const time = d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
  return d.toDateString() === new Date().toDateString() ? time : `${d.toLocaleDateString([], { weekday: 'short' })} ${time}`
}

/** Plan usage limits (same as Claude's "Plan usage limits" panel), refreshed every 2 minutes. */
function UsageMeters(): React.JSX.Element | null {
  const [limits, setLimits] = useState<UsageLimit[] | null>(null)
  useEffect(() => {
    let live = true
    const load = (): void => {
      // optional call: an app started before this feature has no `usage` bridge until it restarts
      window.api.claude.usage?.().then((l) => live && l && setLimits(l)).catch(() => {})
    }
    load()
    const t = setInterval(load, 120000)
    return () => {
      live = false
      clearInterval(t)
    }
  }, [])
  return (
    <div className="usage-meters">
      {limits?.map((l) => (
        <div key={l.kind + l.label} className={`usage-meter sev-${l.severity}`} title={`${l.label}: đã dùng ${l.percent}% · làm mới lúc ${resetLabel(l.resetsAt)}`}>
          <span className="usage-label">{l.label}</span>
          <span className="usage-bar">
            <span style={{ width: `${Math.min(100, l.percent)}%` }} />
          </span>
          <span className="usage-pct">{l.percent}%</span>
          {l.resetsAt && <span className="usage-reset">↻ {resetLabel(l.resetsAt)}</span>}
        </div>
      ))}
    </div>
  )
}

export function StatusBar(): React.JSX.Element {
  const claude = useStore((s) => s.claude)
  const working = useStore((s) => s.chats.filter((c) => c.status === 'working').length)
  const total = useStore((s) => s.chats.length)

  return (
    <div className="statusbar">
      <div className="statusbar-left">
        <UpdateButton />
        {total > 0 && (
          <button className="sb-item" onClick={() => useStore.setState({ sidebarView: 'chats', sidebarOpen: true })}>
            {working > 0 ? <Loader2 size={12} className="spin" /> : <MessagesSquare size={12} />}
            {working > 0 ? `${working} đang làm` : `${total} chat`}
          </button>
        )}
      </div>
      <UsageMeters />
      <div className="statusbar-right">
        {claude && <AgentPicker />}
      </div>
    </div>
  )
}

/** Status-bar button shown when GitHub has a newer release: click to download and install it. */
function UpdateButton(): React.JSX.Element | null {
  const [version, setVersion] = useState<string | null>(null)
  const [state, setState] = useState<'idle' | 'busy' | 'failed'>('idle')

  useEffect(() => {
    const check = (): void => void window.api.app.checkUpdate().then(setVersion, () => {})
    check()
    const t = setInterval(check, 6 * 60 * 60 * 1000)
    return () => clearInterval(t)
  }, [])

  if (!version) return null
  const install = (): void => {
    setState('busy')
    window.api.app.installUpdate().catch(() => setState('failed'))
  }
  return (
    <button className="sb-item sb-update" disabled={state === 'busy'} onClick={install} title={state === 'failed' ? 'Update failed, click to retry' : `Download and install v${version}`}>
      {state === 'busy' ? <Loader2 size={12} className="spin" /> : <ArrowDownToLine size={12} />}
      {state === 'busy' ? 'Updating…' : state === 'failed' ? 'Update failed · retry' : `Update v${version}`}
    </button>
  )
}

/** Status-bar "AI in use" picker — a plain New chat starts with the agent chosen here. */
function AgentPicker(): React.JSX.Element {
  const claude = useStore((s) => s.claude)
  useStore((s) => s.agents)
  useStore((s) => s.preferredAgent)
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  const list = installedAgents()
  const cur = defaultAgent()

  useEffect(() => {
    if (!open) return
    const close = (e: MouseEvent): void => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false)
    }
    const esc = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setOpen(false)
    }
    window.addEventListener('mousedown', close, true)
    window.addEventListener('keydown', esc, true)
    return () => {
      window.removeEventListener('mousedown', close, true)
      window.removeEventListener('keydown', esc, true)
    }
  }, [open])

  if (!cur)
    return (
      <span className="sb-item warn" title={claude?.path ?? ''}>
        <CircleAlert size={12} /> Không tìm thấy AI
      </span>
    )
  return (
    <div className="sb-agent" ref={ref}>
      <button className="sb-item ok" title="AI đang dùng — chat mới sẽ chạy bằng AI này" onClick={() => setOpen((o) => !o)}>
        <CircleCheck size={12} /> {agentDef(cur).label}
        {list.length > 1 && <ChevronUp size={11} />}
      </button>
      {open && list.length > 0 && (
        <div className="menu sb-agent-menu">
          <div className="menu-sep">AI đang dùng</div>
          {list.map((id) => {
            const a = agentDef(id)
            return (
              <button
                key={id}
                className="menu-item"
                onClick={() => {
                  setDefaultAgent(id)
                  setOpen(false)
                }}
              >
                <span className="menu-item-main">{a.label}</span>
                {id === cur ? <Check size={12} /> : <span className="menu-item-sub">{a.cmd}</span>}
              </button>
            )
          })}
        </div>
      )}
    </div>
  )
}

/* ================================ Quick open ================================ */

/** `q` and `t` already lower-cased. */
function fuzzy(q: string, t: string): number {
  if (!q) return 1
  const name = t.slice(t.lastIndexOf('/') + 1)
  if (name.startsWith(q)) return 1000 - name.length
  if (name.includes(q)) return 800 - name.length
  if (t.includes(q)) return 500 - t.length
  let ti = 0
  let score = 0
  for (const ch of q) {
    const idx = t.indexOf(ch, ti)
    if (idx < 0) return -1
    score += idx === ti ? 3 : 1
    ti = idx + 1
  }
  return score
}

/**
 * Ctrl+P: files. With ">" first (Ctrl+Shift+P / F1): every command and its shortcut.
 * "@" jumps to a symbol in the open file, ":" to a line.
 */
export function QuickOpen(): React.JSX.Element {
  const root = useStore((s) => s.root)
  const [files, setFiles] = useState<string[]>([])
  const [q, setQ] = useState(() => useStore.getState().quickOpenQuery)
  const [sel, setSel] = useState(0)
  const listRef = useRef<HTMLDivElement>(null)
  const commandMode = q.startsWith('>')

  useEffect(() => {
    if (root) window.api.fs.listFiles(root).then(setFiles)
  }, [root])

  // lower-cased once per project, not once per file per keystroke
  const lower = useMemo(() => files.map((f) => f.toLowerCase()), [files])
  // typing stays responsive in big projects: the list catches up a frame later
  const dq = useDeferredValue(q)
  const fileHits = useMemo(() => {
    if (dq.startsWith('>')) return []
    const needle = dq.trim().toLowerCase()
    if (!needle) return files.slice(0, 60)
    const hits: { f: string; s: number }[] = []
    for (let i = 0; i < files.length; i++) {
      const sc = fuzzy(needle, lower[i])
      if (sc >= 0) hits.push({ f: files[i], s: sc })
    }
    return hits
      .sort((a, b) => b.s - a.s)
      .slice(0, 60)
      .map((x) => x.f)
  }, [files, lower, dq])
  const commandHits = useMemo(() => {
    if (!commandMode) return []
    const needle = q.slice(1).trim().toLowerCase()
    if (!needle) return COMMANDS
    return COMMANDS.map((c) => ({ c, s: fuzzy(needle, c.label.toLowerCase()) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .map((x) => x.c)
  }, [commandMode, q])
  const count = commandMode ? commandHits.length : fileHits.length

  useEffect(() => setSel(0), [q])
  useEffect(() => {
    revealIn(listRef.current, listRef.current?.children[sel], { block: 'nearest' })
  }, [sel])

  const close = (): void => useStore.setState({ quickOpen: false, quickOpenQuery: '' })
  const pickFile = (rel: string): void => {
    if (!root) return
    const sep = window.api.platform === 'win32' ? '\\' : '/'
    openFile(root.replace(/[\\/]$/, '') + sep + rel.replace(/[\\/]/g, sep))
    close()
  }
  const runCommand = (c: Command): void => {
    close()
    // after the palette is gone, so focus lands where the command puts it
    setTimeout(c.run)
  }
  const pick = (i: number): void => {
    if (commandMode) {
      if (commandHits[i]) runCommand(commandHits[i])
    } else if (fileHits[i]) pickFile(fileHits[i])
  }

  return (
    <div className="overlay" onMouseDown={close}>
      <div className="palette" onMouseDown={(e) => e.stopPropagation()}>
        <div className="palette-input">
          <Search size={15} />
          <input
            autoFocus
            placeholder={commandMode ? 'Gõ tên lệnh…' : 'Tìm tệp theo tên…  (gõ > để xem lệnh)'}
            value={q}
            onChange={(e) => {
              setQ(e.target.value)
            }}
            onKeyDown={(e) => {
              if (e.key === 'Escape') close()
              if (e.key === 'ArrowDown') {
                e.preventDefault()
                setSel((s) => Math.min(s + 1, count - 1))
              }
              if (e.key === 'ArrowUp') {
                e.preventDefault()
                setSel((s) => Math.max(s - 1, 0))
              }
              if (e.key === 'Enter') pick(sel)
            }}
          />
        </div>
        <div className="palette-list" ref={listRef}>
          {commandMode
            ? commandHits.map((c, i) => (
                <div key={c.id} className={`palette-row${i === sel ? ' sel' : ''}`} onMouseEnter={() => setSel(i)} onClick={() => runCommand(c)}>
                  <span className="palette-name">{c.label}</span>
                  {c.keys && <kbd className="palette-keys">{c.keys}</kbd>}
                </div>
              ))
            : fileHits.map((r, i) => {
                const name = r.split('/').pop()!
                return (
                  <div key={r} className={`palette-row${i === sel ? ' sel' : ''}`} onMouseEnter={() => setSel(i)} onClick={() => pickFile(r)}>
                    <FileIcon name={name} />
                    <span className="palette-name">{name}</span>
                    <span className="palette-dir">{r.slice(0, -name.length - 1)}</span>
                  </div>
                )
              })}
          {count === 0 && (
            <div className="palette-empty">{commandMode ? 'Không có lệnh nào khớp' : !root ? 'Hãy mở một thư mục trước' : files.length ? 'Không có tệp nào khớp' : 'Đang lập chỉ mục…'}</div>
          )}
        </div>
      </div>
    </div>
  )
}

/* ================================ Welcome (no folder) ================================ */

export function Welcome(): React.JSX.Element {
  const claude = useStore((s) => s.claude)
  const projects = useStore((s) => s.projects)
  const byRecent = [...projects].sort((a, b) => b.updated - a.updated)
  const totalChats = projects.reduce((n, g) => n + g.sessions.length, 0)
  return (
    <div className="welcome">
      <div className="welcome-bg" />
      <div className="welcome-grid">
        <div className="welcome-inner">
          <div className="glow-orb big">
            <Logo size={84} />
          </div>
          <h1>
            VIBE <span className="h1-accent">CODE</span>
          </h1>
          <p className="welcome-sub">Không gian làm việc với Claude cho thiết kế và sự kiện: nhiều chat song song, trình duyệt, Zalo và chụp màn hình có chú thích.</p>
          <div className="welcome-actions">
            <button className="btn btn-primary btn-lg" onClick={() => useStore.setState({ centerView: 'events' })}>
              <CalendarDays size={17} /> Mở Sự kiện
            </button>
            <button className="btn btn-lg" onClick={() => openFolder()}>
              <FolderOpen size={17} /> Mở thư mục dự án
            </button>
          </div>
          {claude && !claude.found && (
            <p className="welcome-warn">
              <CircleAlert size={14} /> Máy chưa có Claude Code. Mở một thư mục để xem hướng dẫn cài.
            </p>
          )}
        </div>

        {projects.length > 0 && (
          <div className="claude-projects">
            <div className="cp-head">
              <span className="cp-title">
                <Link2 size={13} /> Từ Claude
              </span>
              <span className="cp-sub">
                {projects.length} dự án · {totalChats} chat
              </span>
            </div>
            <div className="cp-list">
              {byRecent.map((g) => (
                <div key={g.key} className="cp-proj">
                  <button className="cp-proj-row" onClick={() => openFolder(g.cwd, { autoChat: false })} title={g.cwd}>
                    <FolderOpen size={15} />
                    <span className="cp-name">{g.name}</span>
                    <span className="cp-count">{g.sessions.length}</span>
                    <span className="cp-path">{g.cwd}</span>
                  </button>
                  {g.sessions.slice(0, 2).map((s) => (
                    <button key={s.id} className="cp-sess" onClick={() => openSession(s)} title={`Tiếp tục: ${s.title}`}>
                      <MessagesSquare size={12} />
                      <span className="cp-sess-title">{s.title}</span>
                      <span className="cp-sess-time">{ago(s.updated)}</span>
                    </button>
                  ))}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

function ago(t: number): string {
  const m = Math.floor((Date.now() - t) / 60000)
  if (m < 60) return `${Math.max(1, m)}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h`
  return `${Math.floor(h / 24)}d`
}
