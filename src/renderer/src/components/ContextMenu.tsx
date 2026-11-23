import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { Clipboard, ClipboardPaste, Copy, Scissors, TextSelect } from 'lucide-react'

export type MenuEntry = { label: string; icon?: React.ReactNode; action: () => unknown; disabled?: boolean; hint?: string } | 'sep' | null | undefined | false

interface Open {
  x: number
  y: number
  items: MenuEntry[]
}

let show: ((m: Open | null) => void) | null = null

/** Open a right-click menu at the mouse. Falsy entries are skipped, so items can be added conditionally. */
export function openMenu(e: { clientX: number; clientY: number; target?: EventTarget; preventDefault(): void; stopPropagation(): void }, items: MenuEntry[]): void {
  // inside a text box (a name being renamed…) the box's own Cut / Copy / Paste menu wins
  if ((e.target as HTMLElement | undefined)?.closest?.('input, textarea')) return
  e.preventDefault()
  e.stopPropagation()
  show?.({ x: e.clientX, y: e.clientY, items })
}

/** Run `f` with the text field focused, so execCommand acts on it. */
function inField(el: HTMLInputElement | HTMLTextAreaElement | HTMLElement, f: () => void): () => void {
  return () => {
    el.focus()
    f()
  }
}

/** Cut / Copy / Paste / Select all for a text field. */
function editItems(el: HTMLInputElement | HTMLTextAreaElement | HTMLElement): MenuEntry[] {
  const ro = 'readOnly' in el ? el.readOnly || el.disabled : !el.isContentEditable
  const sel = 'selectionStart' in el && el.selectionStart !== null ? el.selectionStart !== el.selectionEnd : !!window.getSelection()?.toString()
  return [
    { label: 'Cắt', icon: <Scissors size={14} />, hint: 'Ctrl+X', disabled: ro || !sel, action: inField(el, () => document.execCommand('cut')) },
    { label: 'Sao chép', icon: <Copy size={14} />, hint: 'Ctrl+C', disabled: !sel, action: inField(el, () => document.execCommand('copy')) },
    {
      label: 'Dán',
      icon: <ClipboardPaste size={14} />,
      hint: 'Ctrl+V',
      disabled: ro,
      // insertText keeps undo and fires input events, so React state follows
      action: async () => {
        const text = await navigator.clipboard.readText().catch(() => '')
        el.focus()
        if (text) document.execCommand('insertText', false, text)
      }
    },
    'sep',
    { label: 'Chọn tất cả', icon: <TextSelect size={14} />, hint: 'Ctrl+A', action: inField(el, () => document.execCommand('selectAll')) }
  ]
}

/**
 * Every right-click the app doesn't handle itself: text fields get Cut / Copy / Paste, selected text
 * gets Copy, and the rest gets nothing (never the WebView's Back / Refresh / Print / Inspect menu).
 */
function onContextMenu(e: MouseEvent): void {
  if (e.defaultPrevented) return
  e.preventDefault()
  const t = e.target as HTMLElement | null
  // Monaco and the terminals bring their own
  if (t?.closest('.monaco-editor, .xterm')) return
  const field = t?.closest<HTMLElement>('input, textarea, [contenteditable=""], [contenteditable="true"]')
  const textInput = field && (field.tagName !== 'INPUT' || /^(text|search|url|email|password|number|tel|)$/i.test((field as HTMLInputElement).type))
  if (field && textInput) return show?.({ x: e.clientX, y: e.clientY, items: editItems(field) })
  const text = window.getSelection()?.toString()
  if (text)
    show?.({ x: e.clientX, y: e.clientY, items: [{ label: 'Sao chép', icon: <Clipboard size={14} />, hint: 'Ctrl+C', action: () => navigator.clipboard.writeText(text) }] })
}

/** Mounted once in App: draws the open menu and takes over the WebView's own right-click menu. */
export function ContextMenuHost(): React.JSX.Element | null {
  const [menu, setMenu] = useState<Open | null>(null)
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState<React.CSSProperties>({})

  useEffect(() => {
    show = setMenu
    window.addEventListener('contextmenu', onContextMenu)
    return () => {
      show = null
      window.removeEventListener('contextmenu', onContextMenu)
    }
  }, [])

  // keep it on screen — measured before paint so it never jumps
  useLayoutEffect(() => {
    if (!menu) return
    setPos({ left: menu.x, top: menu.y, visibility: 'hidden' })
  }, [menu])
  useLayoutEffect(() => {
    const r = ref.current?.getBoundingClientRect()
    if (!menu || !r || pos.visibility !== 'hidden') return
    setPos({
      left: Math.max(4, Math.min(menu.x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(menu.y, window.innerHeight - r.height - 4))
    })
  }, [menu, pos])

  useEffect(() => {
    if (!menu) return
    const close = (): void => setMenu(null)
    const down = (e: MouseEvent): void => {
      if (!ref.current?.contains(e.target as Node)) close()
    }
    const key = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') close()
    }
    window.addEventListener('mousedown', down, true)
    window.addEventListener('keydown', key, true)
    window.addEventListener('blur', close)
    window.addEventListener('resize', close)
    document.addEventListener('scroll', close, true)
    return () => {
      window.removeEventListener('mousedown', down, true)
      window.removeEventListener('keydown', key, true)
      window.removeEventListener('blur', close)
      window.removeEventListener('resize', close)
      document.removeEventListener('scroll', close, true)
    }
  }, [menu])

  const items = menu?.items.filter((x): x is Exclude<MenuEntry, null | undefined | false> => !!x) ?? []
  if (!menu || !items.some((x) => x !== 'sep')) return null
  // portal to <body>: a transformed/blurred ancestor would offset position:fixed
  return createPortal(
    <div
      ref={ref}
      className="menu ctx-menu"
      style={pos}
      // keep focus (and the selection) in the field the menu acts on
      onMouseDown={(e) => e.preventDefault()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) =>
        it === 'sep' ? (
          i > 0 && i < items.length - 1 && items[i - 1] !== 'sep' && <div key={i} className="ctx-sep" />
        ) : (
          <button
            key={i}
            className="menu-item"
            disabled={it.disabled}
            onClick={() => {
              setMenu(null)
              void it.action()
            }}
          >
            {it.icon}
            <span className="menu-item-main">{it.label}</span>
            {it.hint && <span className="menu-item-sub">{it.hint}</span>}
          </button>
        )
      )}
    </div>,
    document.body
  )
}
