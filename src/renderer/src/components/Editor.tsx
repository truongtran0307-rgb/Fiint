import { Suspense, lazy, memo, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { CalendarDays, Camera, Copy, ExternalLink, FolderOpen, X, Globe, MessageCircle, MessageSquarePlus, Plus, Search } from 'lucide-react'
import { TEMPLATES, insertTemplate } from '../templates'
import { useStore } from '../store'
import { HOME_URL, browserKey, closeBrowserTab, newBrowserTab, openBrowser, selectBrowserTab } from '../actions'
import { BrowserTab } from './Browser'
import { SlidePill } from './motion'
import { openMenu } from './ContextMenu'
import { ExplorerView } from './Sidebar'
const ZaloPane = lazy(() => import('./Zalo').then((m) => ({ default: m.ZaloPane })))
const EventsPane = lazy(() => import('./Events').then((m) => ({ default: m.EventsPane })))
import type { ZaloTasksState } from '../../../shared/types'
import { revealIn } from '../scroll'

/** Middle area: fixed tabs — Files (project folder), Web (built-in browser) and Zalo. */
export function EditorArea(): React.JSX.Element {
  const view = useStore((s) => s.centerView)
  const key = useStore((s) => browserKey(s.activeChatId))
  const browsers = useStore((s) => s.browsers)
  // the Zalo tab mounts on first visit, then stays
  const [eventsSeen, setEventsSeen] = useState(view === 'events')
  const [zaloSeen, setZaloSeen] = useState(view === 'zalo')
  useEffect(() => {
    if (view === 'zalo') setZaloSeen(true)
    if (view === 'events') setEventsSeen(true)
  }, [view])
  const zaloBadge = useZaloBadge(view === 'zalo')
  // switching chats while on Web: give that chat its own browser (before paint: no empty frame)
  useLayoutEffect(() => {
    if (view === 'web' && !browsers[key]) openBrowser()
  }, [view, key, browsers])
  return (
    <div className="center-area">
      <div className="center-switch" role="tablist">
        <SlidePill />
        <button role="tab" aria-selected={view === 'code'} className={view === 'code' ? 'on' : ''} onClick={() => useStore.setState({ centerView: 'code' })}>
          <FolderOpen size={14} /> <span className="center-label">Tệp</span>
        </button>
        <button role="tab" aria-selected={view === 'web'} className={view === 'web' ? 'on' : ''} onClick={() => openBrowser()}>
          <Globe size={14} /> <span className="center-label">Web</span>
        </button>
        <button role="tab" aria-selected={view === 'zalo'} className={view === 'zalo' ? 'on' : ''} onClick={() => useStore.setState({ centerView: 'zalo' })}>
          <MessageCircle size={14} /> <span className="center-label">Zalo</span>
          {zaloBadge > 0 && (
            <span key={zaloBadge} className="center-count alert" title={`${zaloBadge} việc mới / vừa cập nhật`}>
              {zaloBadge > 99 ? '99+' : zaloBadge}
            </span>
          )}
        </button>
        <button role="tab" aria-selected={view === 'events'} className={view === 'events' ? 'on' : ''} onClick={() => useStore.setState({ centerView: 'events' })}>
          <CalendarDays size={14} /> <span className="center-label">Sự kiện</span>
        </button>
      </div>
      {/*
        Panes stay mounted and laid out; only the open one (`on`) is visible. display:none would drop
        their scroll positions, make Monaco re-measure on return and replay every animation inside.
      */}
      {/* Files: the project folder on the left (files open in their own app), tips on the right */}
      <div className={`center-pane code-pane${view === 'code' ? ' on' : ''}`}>
        <CodeSide />
        <div className="editor-area">
          <EmptyEditor />
        </div>
      </div>
      {zaloSeen && (
        <div className={`center-pane zalo-host${view === 'zalo' ? ' on' : ''}`}>
          <Suspense fallback={null}>
            <ZaloPane />
          </Suspense>
        </div>
      )}
      {eventsSeen && (
        <div className={view === 'events' ? 'center-pane on' : 'center-pane'} hidden={view !== 'events'} inert={view !== 'events'}>
          <Suspense fallback={null}>
            <EventsPane />
          </Suspense>
        </div>
      )}
      {/* one browser per chat, every tab kept alive so pages survive switching */}
      {Object.keys(browsers).map((k) => (
        <div key={k} className={`center-pane browser-host${view === 'web' && k === key ? ' on' : ''}`}>
          <WebTabs chatKey={k} />
        </div>
      ))}
    </div>
  )
}

/**
 * Zalo tab badge: tasks Claude created or changed that I haven't looked at.
 * Also tells main when the tab is on screen in a focused window (that clears the badge, and skips pop-ups).
 */
function useZaloBadge(onTab: boolean): number {
  const [n, setN] = useState(0)
  useEffect(() => {
    const count = (s: ZaloTasksState): void => {
      setN(Object.keys(s.unseen ?? {}).length)
      taskbarBadge(s.tasks.filter((t) => t.status !== 'done').length)
    }
    void window.api.zalo.tasks().then(count)
    return window.api.zalo.onTasks(count)
  }, [])
  useEffect(() => {
    const send = (): void => window.api.zalo.tasksViewing(onTab && document.hasFocus() && !document.hidden)
    send()
    window.addEventListener('focus', send)
    window.addEventListener('blur', send)
    document.addEventListener('visibilitychange', send)
    return () => {
      window.removeEventListener('focus', send)
      window.removeEventListener('blur', send)
      document.removeEventListener('visibilitychange', send)
    }
  }, [onTab])
  return n
}

/** Red number over the app's taskbar icon: my unfinished Zalo tasks. */
let lastBadge = -1
function taskbarBadge(n: number): void {
  if (n === lastBadge) return
  lastBadge = n
  if (n <= 0) return window.api.win.badge(null, '')
  const c = document.createElement('canvas')
  c.width = c.height = 32
  const g = c.getContext('2d')
  if (!g) return
  g.fillStyle = '#e5484d'
  g.beginPath()
  g.arc(16, 16, 16, 0, Math.PI * 2)
  g.fill()
  const text = n > 99 ? '99+' : String(n)
  g.fillStyle = '#fff'
  g.font = `bold ${text.length > 2 ? 13 : text.length > 1 ? 18 : 22}px "Segoe UI", sans-serif`
  g.textAlign = 'center'
  g.textBaseline = 'middle'
  g.fillText(text, 16, 17)
  window.api.win.badge(c.toDataURL('image/png'), `${n} việc chưa xong`)
}

/** A chat's browser: its tab strip and one page per tab. */
function WebTabs({ chatKey }: { chatKey: string }): React.JSX.Element | null {
  const b = useStore((s) => s.browsers[chatKey])
  const shown = useStore((s) => s.centerView === 'web' && browserKey(s.activeChatId) === chatKey)
  const stripRef = useRef<HTMLDivElement>(null)
  // tabs on their way out: they shrink away first, then really close
  const [closing, setClosing] = useState<Set<string>>(new Set())
  const close = (id: string): void => {
    if (closing.has(id)) return
    setClosing((c) => new Set(c).add(id))
    setTimeout(() => {
      closeBrowserTab(chatKey, id)
      setClosing((c) => {
        const n = new Set(c)
        n.delete(id)
        return n
      })
    }, 200)
  }

  useEffect(() => {
    revealIn(stripRef.current, stripRef.current?.querySelector('.tab.active'), { inline: 'nearest', smooth: true })
  }, [b?.active, b?.tabs.length])

  if (!b) return null
  const activeIdx = b.tabs.findIndex((t) => t.id === b.active)
  return (
    <>
      <div className="tab-strip web-tab-strip" ref={stripRef} onWheel={(e) => (e.currentTarget.scrollLeft += e.deltaY)}>
        <SlidePill className="tab-pill" />
        {b.tabs.map((t) => (
          <div
            key={t.id}
            className={`tab${t.id === b.active ? ' active' : ''}${closing.has(t.id) ? ' closing' : ''}`}
            onClick={() => selectBrowserTab(chatKey, t.id)}
            onContextMenu={(e) =>
              openMenu(e, [
                { label: 'Đóng tab', icon: <X size={14} />, action: () => close(t.id) },
                b.tabs.length > 1 && { label: 'Đóng các tab khác', action: () => b.tabs.forEach((o) => o.id !== t.id && close(o.id)) },
                t.url !== HOME_URL && 'sep',
                t.url !== HOME_URL && { label: 'Sao chép địa chỉ', icon: <Copy size={14} />, action: () => navigator.clipboard.writeText(t.url) },
                t.url !== HOME_URL && { label: 'Mở trong trình duyệt mặc định', icon: <ExternalLink size={14} />, action: () => window.api.app.openExternal(t.url) }
              ])
            }
            onMouseDown={(e) => {
              if (e.button === 1) {
                e.preventDefault()
                close(t.id)
              }
            }}
            title={t.url === HOME_URL ? 'Tab mới' : t.url}
          >
            <Globe size={13} />
            <span className="tab-title">{t.title || (t.url === HOME_URL ? 'Tab mới' : t.url.replace(/^https?:\/\//, ''))}</span>
            <button
              className="tab-close"
              title="Đóng tab"
              onClick={(e) => {
                e.stopPropagation()
                close(t.id)
              }}
            >
              <X size={13} />
            </button>
          </div>
        ))}
        <button className="icon-btn tab-strip-btn" title="Tab mới" onClick={() => newBrowserTab(chatKey)}>
          <Plus size={15} />
        </button>
        <div className="tab-strip-fill" />
      </div>
      {/* every page stays laid out (no display:none → no re-layout flash); the open one slides in */}
      <div className="browser-deck">
        {b.tabs.map((t, i) => (
          <div key={t.id} className={`browser-tab-pane ${i === activeIdx ? 'on' : i < activeIdx ? 'left' : 'right'}`}>
            <BrowserTab chatKey={chatKey} tabId={t.id} active={t.id === b.active} onScreen={shown} />
          </div>
        ))}
      </div>
    </>
  )
}

/** Left column of the Files tab: the project's folder tree. */
// memo (no props): EditorArea re-renders whenever any web page changes its address or title
const CodeSide = memo(function CodeSide(): React.JSX.Element {
  const hasRoot = useStore((s) => !!s.root)
  return <div className="sidebar code-tree">{hasRoot && <ExplorerView />}</div>
})


/* ================================ Empty state ================================ */

/** The Files tab's right side: event templates to start from, and a few keys worth knowing. */
function EmptyEditor(): React.JSX.Element {
  const rows: [React.ReactNode, string, string][] = [
    [<MessageSquarePlus size={14} key="n" />, 'Chat mới', 'Ctrl+Shift+N'],
    [<Search size={14} key="s" />, 'Tìm tệp trong dự án', 'Ctrl+P'],
    [<Camera size={14} key="z" />, 'Chụp màn hình gửi cho Claude', 'Ctrl+Alt+Z']
  ]
  return (
    <div className="empty-editor quick-start">
      <div className="qs-head">
        <h2>Bắt đầu nhanh</h2>
          <p>Chọn một mẫu việc, điền thông tin trong ô soạn bên phải rồi bấm Gửi.</p>
      </div>
      <div className="qs-grid">
        {TEMPLATES.map((t) => (
          <button key={t.key} className="qs-card" title={t.text} onClick={() => insertTemplate(t.text)}>
            <span className="qs-icon">{t.icon}</span>
            <span className="qs-text">
              <span className="qs-label">{t.label}</span>
              <span className="qs-hint">{t.hint}</span>
            </span>
          </button>
        ))}
      </div>
      <p className="qs-note">Bấm vào tệp ở cột bên trái để mở bằng ứng dụng mặc định (Illustrator, Photoshop, Word, Excel, trình xem ảnh…).</p>
      <div className="shortcut-list">
        {rows.map(([icon, label, key]) => (
          <div key={label} className="shortcut-row">
            <span className="shortcut-label">
              {icon} {label}
            </span>
            <kbd>{key}</kbd>
          </div>
        ))}
      </div>
    </div>
  )
}
