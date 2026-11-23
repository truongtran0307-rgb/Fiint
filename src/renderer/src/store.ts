import { create } from 'zustand'
import type { AgentId } from '../../shared/agents'
import type { AgentInfo, AppSettings, ClaudeInfo, GitState, ProjectGroup, WindowState } from '../../shared/types'

export type ChatStatus = 'starting' | 'working' | 'idle' | 'exited'

export interface Chat {
  draft?: string
  sentDraft?: { text: string; at: number }
  needsInput?: boolean
  id: string
  title: string
  /** true once the user renamed it — stop following the terminal title */
  pinnedTitle: boolean
  cwd: string
  status: ChatStatus
  /** finished work while you were looking elsewhere */
  attention: boolean
  createdAt: number
  /** coding agent this chat runs — undefined = Claude Code */
  agent?: AgentId
  claudeArgs?: string[]
  /** Claude Code session id — links this chat to its entry in ~/.claude/projects */
  sessionId?: string
  model?: string
  effort?: string
  permissionMode?: string
}

export interface ShellTab {
  id: string
  title: string
  cwd: string
  exited: boolean
}

export type EditorTab =
  | { id: string; kind: 'file'; path: string; title: string; dirty: boolean }
  /** commit set: that commit vs its parent; otherwise working tree vs HEAD */
  | { id: string; kind: 'diff'; path: string; rel: string; title: string; commit?: { hash: string; short: string; subject: string } }

export type SidebarView = 'chats' | 'explorer' | 'changes'

interface State {
  root: string | null
  recent: string[]
  claude: ClaudeInfo | null
  /** which coding agents (Claude, Codex, Grok…) are installed */
  agents: AgentInfo[]
  /** AI picked as "in use" — a plain New chat starts with it */
  preferredAgent: AgentId | null
  git: GitState
  fsTick: number
  /** every project + session Claude Code knows about (shared with the Claude app) */
  projects: ProjectGroup[]
  win: WindowState | null
  settings: AppSettings | null
  /** theme actually showing (system preference resolved) */
  themeResolved: 'light' | 'dark' | 'blue'

  sidebarView: SidebarView
  sidebarOpen: boolean
  bottomOpen: boolean
  quickOpen: boolean
  /** what the palette starts with: '' files, '>' commands (Ctrl+Shift+P) */
  quickOpenQuery: string
  /** left column of Source code */
  codeSide: 'files' | 'changes'

  chats: Chat[]
  activeChatId: string | null

  shells: ShellTab[]
  activeShellId: string | null

  tabs: EditorTab[]
  activeTabId: string | null
  diffSideBySide: boolean

  /** middle area: the code editor or the built-in browser */
  centerView: 'code' | 'web' | 'db' | 'zalo' | 'events'
  /** each chat has its own browser tabs: chat id ('_' with no chat open) → its tabs */
  browsers: Record<string, ChatBrowser>
  /** project folder (lower-cased, no trailing slash) → Claude runs it in bypass-permissions mode */
  projectBypass: Record<string, boolean>

  set: (p: Partial<State>) => void
  patchChat: (id: string, p: Partial<Chat>) => void
  patchShell: (id: string, p: Partial<ShellTab>) => void
}

/** One tab of the built-in browser. */
export interface WebTab {
  id: string
  url: string
  title: string
}
export interface ChatBrowser {
  tabs: WebTab[]
  active: string
}

export const useStore = create<State>((set) => ({
  root: null,
  recent: [],
  claude: null,
  agents: [],
  preferredAgent: (() => {
    try {
      return (localStorage.getItem('vibe.newChatAgent') as AgentId | null) ?? null
    } catch {
      return null
    }
  })(),
  git: { isRepo: false, root: null, branch: null, files: [] },
  fsTick: 0,
  projects: [],
  win: null,
  settings: null,
  themeResolved: 'dark',

  sidebarView: 'chats',
  sidebarOpen: true,
  bottomOpen: false,
  quickOpen: false,
  quickOpenQuery: '',
  codeSide: 'files',

  chats: [],
  activeChatId: null,

  shells: [],
  activeShellId: null,

  tabs: [],
  activeTabId: null,
  diffSideBySide: true,

  centerView: 'code',
  browsers: {},
  projectBypass: {},

  set: (p) => set(p),
  // a patch that changes nothing keeps the same array: the terminal title (Claude's spinner) and
  // status checks patch many times a second, and each new array re-rendered every chat list
  patchChat: (id, p) =>
    set((s) => {
      const c = s.chats.find((x) => x.id === id)
      if (!c || (Object.keys(p) as (keyof Chat)[]).every((k) => c[k] === p[k])) return s
      return { chats: s.chats.map((x) => (x.id === id ? { ...x, ...p } : x)) }
    }),
  patchShell: (id, p) =>
    set((s) => {
      const c = s.shells.find((x) => x.id === id)
      if (!c || (Object.keys(p) as (keyof ShellTab)[]).every((k) => c[k] === p[k])) return s
      return { shells: s.shells.map((x) => (x.id === id ? { ...x, ...p } : x)) }
    })
}))

export const uid = (): string => Math.random().toString(36).slice(2, 10) + Date.now().toString(36).slice(-4)

export const basename = (p: string): string => p.split(/[\\/]/).filter(Boolean).pop() ?? p

export const fileUri = (p: string): string => 'file:///' + p.replace(/\\/g, '/').replace(/^\/+/, '')
