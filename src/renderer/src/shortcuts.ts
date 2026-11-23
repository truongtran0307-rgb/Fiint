/**
 * Every app command with its keyboard shortcut, in one table: the global key handler and the
 * command palette (Ctrl+Shift+P / F1) both read it, so a shortcut shown in the palette is always
 * the one that works.
 *
 * Bindings follow VS Code (and Windows Terminal for the terminal keys). Keys that mean something
 * inside a terminal — readline (Ctrl+W, Ctrl+L…) and Claude Code (Ctrl+B background, Ctrl+O
 * transcript, Ctrl+J newline, Ctrl+T todos…) — go to the terminal when it has the cursor; the app
 * only takes them elsewhere.
 */
import { useStore } from './store'
import { closeChat, cycleChat, defaultAgent, newChat, openBrowser, openFolder, selectChat, toggleTheme } from './actions'
import { focusTerm } from './terminals'

export interface Command {
  id: string
  label: string
  /** shown in the palette, e.g. "Ctrl+Shift+P" */
  keys?: string
  run: () => void
}

const st = useStore.getState

const palette = (query: string): void => useStore.setState({ quickOpen: true, quickOpenQuery: query })

export const COMMANDS: Command[] = [
  { id: 'palette', label: 'Hiện tất cả lệnh', keys: 'Ctrl+Shift+P', run: () => palette('>') },
  { id: 'quickOpen', label: 'Tìm tệp…', keys: 'Ctrl+P', run: () => st().root && palette('') },
  { id: 'openFolder', label: 'Mở thư mục…', keys: 'Ctrl+O', run: () => void openFolder(null, { autoChat: false }) },

  { id: 'newChat', label: 'Chat mới', keys: 'Ctrl+Shift+N', run: () => void (defaultAgent() && newChat()) },
  { id: 'nextChat', label: 'Chat tiếp theo', keys: 'Ctrl+Alt+→', run: () => cycleChat(1) },
  { id: 'prevChat', label: 'Chat trước', keys: 'Ctrl+Alt+←', run: () => cycleChat(-1) },
  { id: 'focusChat', label: 'Đưa con trỏ vào chat', keys: 'Ctrl+Shift+L', run: () => focusActiveChat() },
  {
    id: 'closeChat',
    label: 'Đóng chat',
    run: () => {
      const id = st().activeChatId
      if (id) closeChat(id)
    }
  },

  { id: 'chats', label: 'Hiện danh sách chat', keys: 'Ctrl+Shift+C', run: () => useStore.setState({ sidebarView: 'chats', sidebarOpen: true }) },
  { id: 'sidebar', label: 'Ẩn/hiện thanh bên', keys: 'Ctrl+B', run: () => useStore.setState({ sidebarOpen: !st().sidebarOpen }) },

  { id: 'viewFiles', label: 'Hiện tab Tệp', keys: 'Ctrl+1', run: () => useStore.setState({ centerView: 'code' }) },
  { id: 'viewWeb', label: 'Hiện tab Web', keys: 'Ctrl+2', run: () => openBrowser() },
  { id: 'viewZalo', label: 'Hiện tab Zalo', keys: 'Ctrl+3', run: () => useStore.setState({ centerView: 'zalo' }) },
  { id: 'viewEvents', label: 'Hiện tab Sự kiện', keys: 'Ctrl+4', run: () => useStore.setState({ centerView: 'events' }) },

  { id: 'theme', label: 'Đổi giao diện sáng / tối', run: () => void toggleTheme() }
]

const byId = new Map(COMMANDS.map((c) => [c.id, c]))
const run = (id: string): void => byId.get(id)!.run()

function focusActiveChat(): void {
  const id = st().activeChatId
  if (id) focusTerm(id)
}

/** The cursor is in a terminal (a chat or a shell). */
const inTerminal = (): boolean => !!(document.activeElement as HTMLElement | null)?.closest?.('.xterm')
/** The cursor is in a text field of the page (not Monaco, which handles its own keys). */
const inTextField = (): boolean => {
  const el = document.activeElement as HTMLElement | null
  return !!el && !el.closest('.monaco-editor') && !el.closest('.xterm') && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable)
}

/**
 * The command a key press means, or null to let it through. `e.code` is used for keys whose
 * character changes with the keyboard layout (` and digits).
 */
function commandFor(e: KeyboardEvent): string | null {
  const ctrl = e.ctrlKey || e.metaKey
  const { shiftKey: shift, altKey: alt } = e
  const k = e.key.toLowerCase()
  const term = inTerminal()

  if (e.key === 'F1' && !ctrl && !alt) return 'palette'
  // Alt+1…9: switch chat
  if (alt && !ctrl && !shift && /^Digit[1-9]$/.test(e.code)) return `chat:${e.code.slice(5)}`
  if (!ctrl) return null

  if (alt && !shift) {
    if (e.key === 'ArrowRight') return 'nextChat'
    if (e.key === 'ArrowLeft') return 'prevChat'
    return null
  }
  if (alt) return null

  if (shift) {
    switch (k) {
      case 'p':
        return 'palette'
      case 'n':
        return 'newChat'
      case 'l':
        return 'focusChat'
      // Ctrl+Shift+C in a terminal copies (Windows Terminal / VS Code)
      case 'c':
        return term ? null : 'chats'
    }
    return null
  }

  // Ctrl+1…3: the center tabs (in a terminal too: shells don't use them)
  if (/^Digit[1-3]$/.test(e.code)) return ['viewFiles', 'viewWeb', 'viewZalo'][Number(e.code.slice(5)) - 1]
  if (k === 'p') return 'quickOpen'
  // the rest mean something to readline / Claude Code: theirs while a terminal has the cursor
  if (term) return null
  switch (k) {
    case 'b':
      return 'sidebar'
    case 'o':
      return 'openFolder'
  }
  return null
}

/** The page is a web view: its reload keys would wipe every chat's screen. */
function isReload(e: KeyboardEvent): boolean {
  const ctrl = e.ctrlKey || e.metaKey
  return e.key === 'F5' || (ctrl && e.key.toLowerCase() === 'r')
}

export function onGlobalKey(e: KeyboardEvent): void {
  if (e.defaultPrevented) return
  if (isReload(e) && !inTerminal()) {
    // Ctrl+Shift+R still reloads while developing
    if (!(import.meta.env.DEV && e.shiftKey)) e.preventDefault()
    return
  }
  const id = commandFor(e)
  if (!id) return
  // typing in a field (address bar, search box…): only app-wide combos, not plain Ctrl+letters it edits with
  if (inTextField() && ['openFolder', 'sidebar'].includes(id)) return
  e.preventDefault()
  e.stopPropagation()
  if (id.startsWith('chat:')) {
    const c = st().chats[Number(id.slice(5)) - 1]
    if (c) selectChat(c.id)
    return
  }
  run(id)
}
