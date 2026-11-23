/**
 * One tab of the built-in browser (the Web tab). The page itself is a native WebView2 child window
 * that the Rust side keeps over the `.browser-view` box below: this component tells it where that box
 * is, when it is on screen, and drives its address bar, history buttons and screen-size presets.
 *
 * The native page draws above the HTML, so it is parked while anything has to show over it
 * (address suggestions, the bookmarks menu, the command palette).
 */
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { ArrowLeft, ArrowRight, Bookmark, ExternalLink, Folder, Laptop, Maximize, Monitor, RotateCw, Search, Shuffle, Smartphone, Trash2, X } from 'lucide-react'
import { HOME_URL, setBrowserUrl } from '../actions'
import { useStore } from '../store'
import type { WebNav } from '../api'
import type { WebBookmark, WebData, WebSuggestion } from '../../../shared/types'

const MOBILE_UA =
  'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1'

interface Device {
  key: string
  label: string
  icon: React.ReactNode
  w?: number
  h?: number
  ua?: string
}

const DEVICES: Device[] = [
  { key: 'fit', label: 'Vừa khung', icon: <Maximize size={13} /> },
  { key: 'fhd', label: 'Full HD 1920×1080', icon: <Monitor size={13} />, w: 1920, h: 1080 },
  { key: 'laptop', label: 'Laptop 1366×768', icon: <Laptop size={13} />, w: 1366, h: 768 },
  { key: 'mobile', label: 'Điện thoại 390×844', icon: <Smartphone size={13} />, w: 390, h: 844, ua: MOBILE_UA }
]

/** What was typed in an address box → a URL: an address as is, anything else a Google search. */
export function toUrl(input: string): string {
  const q = input.trim()
  if (!q) return ''
  if (/^https?:\/\//i.test(q)) return q
  if (/^(localhost|127\.0\.0\.1)(:\d+)?(\/.*)?$/i.test(q)) return 'http://' + q
  if (!/\s/.test(q) && /^[\w-]+(\.[\w-]+)+(:\d+)?(\/.*)?$/.test(q)) return 'https://' + q
  return 'https://www.google.com/search?q=' + encodeURIComponent(q)
}

const randomPhoto = (): string => `https://picsum.photos/seed/${Math.random().toString(36).slice(2, 8)}/1920/1080`

export function BrowserTab({ chatKey, tabId, active, onScreen }: { chatKey: string; tabId: string; active: boolean; onScreen: boolean }): React.JSX.Element {
  const url = useStore((s) => s.browsers[chatKey]?.tabs.find((t) => t.id === tabId)?.url ?? HOME_URL)
  const quickOpen = useStore((s) => s.quickOpen)
  const label = `web-${tabId}`
  const [opened, setOpened] = useState(false)
  const [nav, setNav] = useState<WebNav>({ url, title: '', back: false, fwd: false, loading: false })
  const [address, setAddress] = useState(url === HOME_URL ? '' : url)
  const [device, setDevice] = useState<Device>(DEVICES[0])
  const [overlay, setOverlay] = useState(false)
  const [stage, setStage] = useState({ w: 0, h: 0 })
  const [clearing, setClearing] = useState(false)
  const viewRef = useRef<HTMLDivElement>(null)
  const stageRef = useRef<HTMLDivElement>(null)
  /** the URL the page is on (as it last reported, or as we sent it) */
  const pageUrl = useRef('')
  const editing = useRef(false)

  // the tab's URL in the store changed (new tab, address typed, a link, Claude…): open or go there
  useEffect(() => {
    if (url === HOME_URL) return
    if (!opened) {
      pageUrl.current = url
      window.api.browser.open(label, chatKey, url, device.ua).then(
        () => setOpened(true),
        () => {}
      )
      return
    }
    if (url !== pageUrl.current) {
      pageUrl.current = url
      void window.api.browser.navigate(label, url).catch(() => {})
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [url, opened])

  // what the page reports: address, title, history, loading
  useEffect(
    () =>
      window.api.browser.onNav((l, n) => {
        if (l !== label) return
        setNav(n)
        if (!n.url || n.url === 'about:blank') return
        pageUrl.current = n.url
        if (!editing.current) setAddress(n.url)
        setBrowserUrl(chatKey, n.url, tabId, n.title || undefined)
        if (!n.loading) window.api.browser.visit(n.url, n.title || undefined)
        if (n.title) window.api.browser.title(n.url, n.title)
      }),
    [label, chatKey, tabId]
  )

  // closing the tab closes its page
  useEffect(() => () => window.api.browser.close(label), [label])

  // Claude's browser tools drive the chat's open tab
  useEffect(() => {
    if (active && opened) window.api.browser.setActive(chatKey, label)
  }, [active, opened, chatKey, label])

  // the space the page may take (for the emulated screen sizes)
  useLayoutEffect(() => {
    const el = stageRef.current
    if (!el) return
    const ro = new ResizeObserver(() => setStage({ w: el.clientWidth, h: el.clientHeight }))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  const emulated = !!device.w && !!device.h
  const zoom = emulated && stage.w > 0 ? Math.min(1, (stage.w - 24) / device.w!, (stage.h - 24) / device.h!) : 1
  const visible = opened && active && onScreen && !quickOpen && !overlay && url !== HOME_URL

  // keep the native page over its box
  useLayoutEffect(() => {
    const el = viewRef.current
    if (!el || !opened) return
    let raf = 0
    const place = (): void => {
      cancelAnimationFrame(raf)
      raf = requestAnimationFrame(() => {
        const r = el.getBoundingClientRect()
        window.api.browser.place(label, { x: r.left, y: r.top, w: r.width, h: r.height }, zoom, visible && r.width > 0 && r.height > 0)
      })
    }
    place()
    // the tab pane slides in (0.3 s): place again once it has settled
    const settle = setTimeout(place, 340)
    const ro = new ResizeObserver(place)
    ro.observe(el)
    window.addEventListener('resize', place)
    return () => {
      cancelAnimationFrame(raf)
      clearTimeout(settle)
      ro.disconnect()
      window.removeEventListener('resize', place)
    }
  }, [label, opened, visible, zoom])

  const go = (input: string): void => {
    const u = toUrl(input)
    if (!u) return
    editing.current = false
    setAddress(u)
    if (u === url && opened) void window.api.browser.navigate(label, u).catch(() => {})
    else setBrowserUrl(chatKey, u, tabId)
  }

  const pickDevice = (d: Device): void => {
    if (d.ua !== device.ua && opened) void window.api.browser.setUserAgent(label, d.ua ?? null).catch(() => {})
    setDevice(d)
  }

  const clearCache = async (): Promise<void> => {
    setClearing(true)
    await window.api.app.clearBrowserCache().catch(() => {})
    if (opened) window.api.browser.reload(label, true)
    setClearing(false)
  }

  const home = url === HOME_URL
  return (
    <div className="browser-tab">
      <div className="browser-bar">
        <button className="icon-btn" title="Quay lại" disabled={!nav.back} onClick={() => window.api.browser.back(label)}>
          <ArrowLeft size={15} />
        </button>
        <button className="icon-btn" title="Tiến tới" disabled={!nav.fwd} onClick={() => window.api.browser.forward(label)}>
          <ArrowRight size={15} />
        </button>
        {nav.loading ? (
          <button className="icon-btn" title="Dừng tải" onClick={() => window.api.browser.stop(label)}>
            <X size={15} />
          </button>
        ) : (
          <button className="icon-btn" title="Tải lại" disabled={home} onClick={() => window.api.browser.reload(label)}>
            <RotateCw size={14} />
          </button>
        )}
        <AddressBox
          value={address}
          onChange={(v) => {
            editing.current = true
            setAddress(v)
          }}
          onGo={go}
          onBlur={() => {
            editing.current = false
            if (!home) setAddress(pageUrl.current || url)
          }}
          setOverlay={setOverlay}
        />
        <div className="device-seg">
          {DEVICES.map((d) => (
            <button key={d.key} className={d.key === device.key ? 'on' : ''} title={d.label} onClick={() => pickDevice(d)}>
              {d.icon}
            </button>
          ))}
        </div>
        {emulated && zoom < 1 && (
          <span className="device-zoom" title="Đã thu nhỏ cho vừa">
            {Math.round(zoom * 100)}%
          </span>
        )}
        <button className="icon-btn" title="Xoá bộ nhớ đệm và tải lại (giữ đăng nhập)" disabled={clearing} onClick={clearCache}>
          <Trash2 size={14} />
        </button>
        <button className="icon-btn" title="Mở trong trình duyệt của bạn" disabled={home} onClick={() => window.api.app.openExternal(pageUrl.current || url)}>
          <ExternalLink size={14} />
        </button>
      </div>
      <BookmarkBar onOpen={go} setOverlay={setOverlay} />
      <div className={`browser-stage${emulated ? ' emulated' : ''}`} ref={stageRef}>
        <div
          className="browser-view"
          ref={viewRef}
          style={emulated ? { flex: 'none', width: device.w! * zoom, height: device.h! * zoom } : undefined}
        />
        {home && <StartPage onGo={go} />}
      </div>
    </div>
  )
}

/** The address bar, with suggestions from history and bookmarks as you type. */
function AddressBox(props: {
  value: string
  onChange: (v: string) => void
  onGo: (v: string) => void
  onBlur: () => void
  setOverlay: (on: boolean) => void
}): React.JSX.Element {
  const { value, onChange, onGo, onBlur, setOverlay } = props
  const [focused, setFocused] = useState(false)
  const [list, setList] = useState<WebSuggestion[]>([])
  const [sel, setSel] = useState(-1)

  useEffect(() => {
    if (!focused || !value.trim()) return setList([])
    let live = true
    const t = setTimeout(() => {
      window.api.browser
        .suggest(value)
        .then((s) => live && setList(s.slice(0, 8)))
        .catch(() => {})
    }, 120)
    return () => {
      live = false
      clearTimeout(t)
    }
  }, [value, focused])

  const open = focused && list.length > 0
  useEffect(() => {
    setOverlay(open)
  }, [open, setOverlay])
  useEffect(() => setSel(-1), [list])

  return (
    <div className="browser-address-wrap">
      <input
        className="browser-address"
        value={value}
        placeholder="Tìm kiếm hoặc nhập địa chỉ"
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
        onFocus={(e) => {
          setFocused(true)
          e.target.select()
        }}
        onBlur={() => {
          setFocused(false)
          onBlur()
        }}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && open) {
            e.preventDefault()
            setSel((s) => Math.min(s + 1, list.length - 1))
          } else if (e.key === 'ArrowUp' && open) {
            e.preventDefault()
            setSel((s) => Math.max(s - 1, -1))
          } else if (e.key === 'Enter') {
            onGo(sel >= 0 ? list[sel].url : value)
            ;(e.target as HTMLInputElement).blur()
          } else if (e.key === 'Escape') {
            ;(e.target as HTMLInputElement).blur()
          }
        }}
      />
      {open && (
        <div className="browser-sugg">
          {list.map((s, i) => (
            <button
              key={s.url}
              className={i === sel ? 'on' : ''}
              // mousedown: before the input's blur closes the list
              onMouseDown={(e) => {
                e.preventDefault()
                onGo(s.url)
                ;(document.activeElement as HTMLElement | null)?.blur()
              }}
            >
              <span className="bs-title">
                {s.bookmark && <Bookmark size={11} />}
                {s.title || s.url}
              </span>
              <span className="bs-url">{s.url}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

const favicon = (url: string): string => {
  try {
    return `https://www.google.com/s2/favicons?sz=32&domain=${new URL(url).hostname}`
  } catch {
    return ''
  }
}

/** Every link inside a bookmark folder, with the folder path it sits in. */
function flatten(items: WebBookmark[], path = ''): { b: WebBookmark; path: string }[] {
  return items.flatMap((b) => (b.url ? [{ b, path }] : flatten(b.children ?? [], path ? `${path} / ${b.title}` : b.title)))
}

/** Bookmarks bar, imported once from Chrome (a button offers it until then). */
function BookmarkBar({ onOpen, setOverlay }: { onOpen: (url: string) => void; setOverlay: (on: boolean) => void }): React.JSX.Element | null {
  const [data, setData] = useState<WebData | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [menu, setMenu] = useState<{ x: number; y: number; items: WebBookmark[] } | null>(null)

  useEffect(() => {
    window.api.browser.data().then(setData, () => {})
  }, [])
  useEffect(() => {
    setOverlay(!!menu)
  }, [menu, setOverlay])

  if (!data) return null
  if (!data.bookmarks.length) {
    if (!data.chromeFound || data.importedAt) return null
    return (
      <div className="bm-bar bm-import">
        <span>Dùng dấu trang của Chrome trong tab Web?</span>
        <button
          className="btn btn-ghost btn-sm"
          disabled={busy}
          onClick={async () => {
            setBusy(true)
            setError('')
            try {
              setData(await window.api.browser.importChrome())
            } catch (e) {
              setError(String(e))
            }
            setBusy(false)
          }}
        >
          Nhập dấu trang từ Chrome
        </button>
        {error && <span className="bm-error">{error}</span>}
      </div>
    )
  }
  return (
    <div className="bm-bar">
      {data.bookmarks.map((b, i) =>
        b.url ? (
          <button key={i} className="bm-item" title={b.url} onClick={() => onOpen(b.url!)}>
            <img className="bm-icon" src={favicon(b.url)} alt="" />
            <span>{b.title || b.url}</span>
          </button>
        ) : (
          <button
            key={i}
            className={`bm-item${menu?.items === b.children ? ' on' : ''}`}
            onClick={(e) => {
              const r = e.currentTarget.getBoundingClientRect()
              setMenu({ x: Math.min(r.left, window.innerWidth - 344), y: r.bottom + 4, items: b.children ?? [] })
            }}
          >
            <Folder size={13} />
            <span>{b.title}</span>
          </button>
        )
      )}
      {menu && (
        <>
          <div className="bm-scrim" onMouseDown={() => setMenu(null)} />
          <div className="bm-menu" style={{ left: menu.x, top: menu.y }}>
            {flatten(menu.items).map(({ b, path }, i) => (
              <button
                key={i}
                title={b.url}
                onClick={() => {
                  setMenu(null)
                  onOpen(b.url!)
                }}
              >
                <img className="bm-icon" src={favicon(b.url!)} alt="" />
                <span className="bm-menu-title">{b.title || b.url}</span>
                {path && <span className="bm-menu-path">{path}</span>}
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  )
}

/** A blank tab: clock, date and a search box over a photo. */
function StartPage({ onGo }: { onGo: (q: string) => void }): React.JSX.Element {
  const [now, setNow] = useState(() => new Date())
  const [q, setQ] = useState('')
  const [photo, setPhoto] = useState(randomPhoto)
  const [loaded, setLoaded] = useState('')
  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 10_000)
    return () => clearInterval(t)
  }, [])
  // fade the photo in once it has loaded
  useEffect(() => {
    const img = new Image()
    img.onload = () => setLoaded(photo)
    img.src = photo
  }, [photo])
  return (
    <div className="start-page">
      <div className={`start-photo${loaded === photo ? ' on' : ''}`} style={{ backgroundImage: `url(${photo})` }} />
      <div className="start-content">
        <div className="start-clock">{now.toLocaleTimeString('vi-VN', { hour: '2-digit', minute: '2-digit' })}</div>
        <div className="start-date">{now.toLocaleDateString('vi-VN', { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' })}</div>
        <form
          className="start-search"
          onSubmit={(e) => {
            e.preventDefault()
            onGo(q)
          }}
        >
          <Search size={17} />
          <input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder="Tìm trên Google hoặc nhập địa chỉ" spellCheck={false} />
        </form>
      </div>
      <button className="start-shuffle" title="Ảnh khác" onClick={() => setPhoto(randomPhoto())}>
        <Shuffle size={15} />
      </button>
    </div>
  )
}
