/**
 * `window.api`: the renderer's only door to the Rust backend (src-tauri).
 *
 * Same shape the UI always used; every call is a Tauri command (`invoke`), every `on…` a Tauri
 * event whose payload is the argument list. Terminal output is the hot path, so it streams
 * through one `Channel` per terminal instead of the global event bus.
 */
import { Channel, invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import type {
  AgentInfo,
  AppSettings,
  ChatFile,
  ChatMessage,
  ChatTask,
  ClaudeInfo,
  DbColumn,
  DbConnection,
  DbConnectionInput,
  DbFound,
  DbObject,
  DbQueryResult,
  DirEntry,
  GitCommit,
  GitBranch,
  GitCommitFile,
  GitState,
  ProjectGroup,
  PtySpawnOptions,
  UsageLimit,
  WebData,
  WebSuggestion,
  WindowMode,
  WindowState,
  ZaloState,
  ZaloTask,
  ZaloTasksState,
  ZaloPhoneInfo
} from '../../shared/types'

type Off = () => void

/** Subscribe to a backend event; the payload is the listener's argument list. */
function on<T extends unknown[]>(channel: string, cb: (...args: T) => void): Off {
  let off: UnlistenFn | null = null
  let dead = false
  void listen<T>(channel, (e) => cb(...e.payload)).then((u) => (dead ? u() : (off = u)))
  return () => {
    dead = true
    off?.()
  }
}

/* ---------------- terminals: one Channel per process ---------------- */

type PtyMsg = { kind: 'data'; data: string } | { kind: 'exit'; code: number }
const ptyData = new Set<(id: string, data: string) => void>()
const ptyExit = new Set<(id: string, code: number) => void>()
const exitNow = (id: string, code: number): void => ptyExit.forEach((cb) => cb(id, code))
const sub = <F>(set: Set<F>, cb: F): Off => {
  set.add(cb)
  return () => set.delete(cb)
}

/** A page in a chat's Web tab: what the address bar / buttons show. */
export interface WebNav {
  url: string
  title: string
  back: boolean
  fwd: boolean
  loading: boolean
}

export interface DropEvent {
  kind: 'enter' | 'over' | 'drop' | 'leave'
  paths: string[]
  /** CSS pixels in the window */
  x: number
  y: number
}

const platform: 'win32' | 'darwin' | 'linux' = navigator.userAgent.includes('Windows') ? 'win32' : navigator.userAgent.includes('Mac') ? 'darwin' : 'linux'

export const api = {
  platform,
  pty: {
    spawn: (o: PtySpawnOptions): Promise<void> => {
      const ch = new Channel<PtyMsg>()
      ch.onmessage = (m) => (m.kind === 'data' ? ptyData.forEach((cb) => cb(o.id, m.data)) : exitNow(o.id, m.code))
      return invoke('pty_spawn', { opts: o, out: ch })
    },
    write: (id: string, data: string): void => {
      void invoke<boolean>('pty_write', { id, data }).then((ok) => ok || exitNow(id, -1))
    },
    resize: (id: string, cols: number, rows: number): void => void invoke('pty_resize', { id, cols, rows }),
    kill: (id: string): void => void invoke('pty_kill', { id }),
    /** on screen → output every 5 ms; off screen → batched every 50 ms (far fewer wake-ups) */
    visible: (id: string, visible: boolean): void => void invoke('pty_visible', { id, visible }),
    onData: (cb: (id: string, data: string) => void): Off => sub(ptyData, cb),
    onExit: (cb: (id: string, code: number) => void): Off => sub(ptyExit, cb)
  },
  claude: {
    detect: (): Promise<ClaudeInfo> => invoke('claude_detect'),
    agents: (): Promise<AgentInfo[]> => invoke('agents_detect'),
    usage: (): Promise<UsageLimit[] | null> => invoke('claude_usage'),
    memory: (): Promise<{ usedBytes: number; totalBytes: number } | null> => invoke('sys_memory'),
    getProjectBypass: (folder: string): Promise<boolean> => invoke('claude_get_project_bypass', { folder }),
    setProjectBypass: (folder: string, on: boolean): Promise<boolean> => invoke('claude_set_project_bypass', { folder, on })
  },
  browser: {
    /** create (hidden) the native page for one Web tab */
    open: (label: string, chatId: string, url: string, userAgent?: string): Promise<void> => invoke('web_open', { label, chatId, url, userAgent }),
    /** where the page sits (CSS px in the window) and its zoom; `visible` false parks it (and lets it sleep) */
    place: (label: string, r: { x: number; y: number; w: number; h: number }, zoom: number, visible: boolean): void =>
      void invoke('web_place', { label, x: r.x, y: r.y, w: r.w, h: r.h, zoom, visible }),
    /** the tab Claude's browser tools drive for this chat */
    setActive: (chatId: string, label: string): void => void invoke('web_set_active', { chatId, label }),
    navigate: (label: string, url: string): Promise<void> => invoke('web_navigate', { label, url }),
    back: (label: string): void => void invoke('web_history', { label, step: -1 }),
    forward: (label: string): void => void invoke('web_history', { label, step: 1 }),
    reload: (label: string, hard = false): void => void invoke('web_reload', { label, hard }),
    stop: (label: string): void => void invoke('web_stop', { label }),
    setUserAgent: (label: string, userAgent: string | null): Promise<void> => invoke('web_set_user_agent', { label, userAgent }),
    close: (label: string): void => void invoke('web_close', { label }),
    onNav: (cb: (label: string, nav: WebNav) => void): Off => on('web:nav', cb),
    /** a chat's browser tools were used: make sure its Web tab exists (and show it) */
    onOpen: (cb: (chatId: string) => void): Off => on('browser:open', cb),
    data: (): Promise<WebData> => invoke('web_data'),
    importChrome: (): Promise<WebData> => invoke('web_import_chrome'),
    suggest: (query: string): Promise<WebSuggestion[]> => invoke('web_suggest', { query }),
    visit: (url: string, title?: string): void => void invoke('web_visit', { url, title }),
    title: (url: string, title: string): void => void invoke('web_title', { url, title })
  },
  sessions: {
    list: (): Promise<ProjectGroup[]> => invoke('sessions_list'),
    /** transcript to the Recycle Bin */
    remove: (sessionId: string): Promise<void> => invoke('sessions_delete', { sessionId }),
    /** `since`: the version already shown — then `images` is null unless a picture was added */
    images: (sessionId: string, since?: number): Promise<{ version: number; images: string[] | null }> => invoke('sessions_images', { sessionId, since }),
    context: (sessionId: string): Promise<{ used: number; model: string } | null> => invoke('sessions_context', { sessionId }),
    tasks: (sessionId: string): Promise<ChatTask[]> => invoke('sessions_tasks', { sessionId }),
    files: (sessionId: string): Promise<ChatFile[]> => invoke('sessions_files', { sessionId }),
    messages: (sessionId: string, since?: number): Promise<{ revision: number; rows: ChatMessage[] | null }> =>
      invoke('sessions_messages', { sessionId, since }),
    /** ids of the sessions whose transcript changed (empty: only project folders came or went) */
    onChanged: (cb: (ids: string[]) => void): Off => on('sessions:changed', cb)
  },
  db: {
    list: (): Promise<DbConnection[]> => invoke('db_list'),
    get: (id: string): Promise<(DbConnection & { conn: string }) | null> => invoke('db_get', { id }),
    save: (c: DbConnectionInput): Promise<DbConnection[]> => invoke('db_save', { c }),
    saveMany: (list: DbConnectionInput[]): Promise<DbConnection[]> => invoke('db_save_many', { list }),
    remove: (id: string): Promise<DbConnection[]> => invoke('db_remove', { id }),
    test: (conn: string): Promise<{ ok: boolean; error?: string; version?: string }> => invoke('db_test', { conn }),
    buildConn: (f: { server: string; database?: string; windowsAuth: boolean; user?: string; password?: string; trustCert?: boolean; encrypt?: boolean }): Promise<string> =>
      invoke('db_build_conn', { f }),
    parseConn: (conn: string): Promise<Record<string, string>> => invoke('db_parse_conn', { conn }),
    query: (id: string, sql: string, database?: string, maxRows?: number): Promise<DbQueryResult> => invoke('db_query', { id, sql, database, maxRows }),
    cancel: (): void => void invoke('db_cancel'),
    databases: (id: string): Promise<string[]> => invoke('db_databases', { id }),
    objects: (id: string, database: string): Promise<DbObject[]> => invoke('db_objects', { id, database }),
    columns: (id: string, database: string, schema: string, table: string): Promise<DbColumn[]> => invoke('db_columns', { id, database, schema, table }),
    scanProject: (folder: string): Promise<DbFound[]> => invoke('db_scan_project', { folder }),
    scanSsms: (): Promise<DbFound[]> => invoke('db_scan_ssms')
  },
  zalo: {
    state: (): Promise<ZaloState> => invoke('zalo_state'),
    login: (): Promise<void> => invoke('zalo_login'),
    cancel: (): Promise<void> => invoke('zalo_cancel'),
    reconnect: (): Promise<ZaloState> => invoke('zalo_reconnect'),
    logout: (): Promise<void> => invoke('zalo_logout'),
    syncAll: (): Promise<number> => invoke('zalo_sync_all'),
    tasks: (): Promise<ZaloTasksState> => invoke('zalo_tasks'),
    analyzeTasks: (): Promise<ZaloTasksState> => invoke('zalo_tasks_analyze'),
    setTaskStatus: (id: string, status: ZaloTask['status']): Promise<ZaloTasksState> => invoke('zalo_task_status', { id, status }),
    taskContext: (id: string): Promise<{ file: string; images: string[]; files: string[] }> => invoke('zalo_task_context', { id }),
    removeTask: (id: string): Promise<ZaloTasksState> => invoke('zalo_task_remove', { id }),
    onTasks: (cb: (s: ZaloTasksState) => void): Off => on('zalo:tasks', cb),
    tasksViewing: (viewing: boolean): void => void invoke('zalo_tasks_viewing', { viewing }),
    /** bảng việc cho iPhone qua Wi-Fi: Mac giữ phiên Zalo, iPhone mở link */
    phoneInfo: (): Promise<ZaloPhoneInfo> => invoke('zalo_phone_info'),
    phoneStart: (): Promise<ZaloPhoneInfo> => invoke('zalo_phone_start'),
    phoneStop: (): Promise<ZaloPhoneInfo> => invoke('zalo_phone_stop'),
    phoneNewLink: (): Promise<ZaloPhoneInfo> => invoke('zalo_phone_new_link'),
    onShowTasks: (cb: () => void): Off => on('zalo:showTasks', cb),
    onState: (cb: (s: ZaloState) => void): Off => on('zalo:state', cb),
    /** names / avatars changed (the task list shows them) */
    onChanged: (cb: (ids: string[]) => void): Off => on('zalo:changed', cb)
  },
  win: {
    state: (): Promise<WindowState> => invoke('win_state'),
    minimize: (): void => void invoke('win_minimize'),
    badge: (png: string | null, label: string): void => void invoke('win_badge', { png, label }),
    toggleMaximize: (): void => void invoke('win_toggle_maximize'),
    close: (): void => void invoke('win_close'),
    hide: (): void => void invoke('win_hide'),
    setMode: (mode: WindowMode): void => void invoke('win_set_mode', { mode }),
    onState: (cb: (s: WindowState) => void): Off => on('win:state', cb),
    onShown: (cb: () => void): Off => on('win:shown', cb)
  },
  fs: {
    readDir: (dir: string): Promise<DirEntry[]> => invoke('fs_read_dir', { dir }),
    readFile: (path: string): Promise<{ content: string; binary: boolean; tooLarge: boolean }> => invoke('fs_read_file', { path }),
    writeFile: (path: string, content: string): Promise<void> => invoke('fs_write_file', { path, content }),
    listFiles: (root: string): Promise<string[]> => invoke('fs_list_files', { root }),
    watch: (root: string | null): Promise<void> => invoke('fs_watch', { root }),
    reveal: (path: string): Promise<void> => invoke('fs_reveal', { path }),
    openPath: (path: string): Promise<string> => invoke('fs_open_path', { path }),
    rename: (from: string, to: string): Promise<void> => invoke('fs_rename', { from, to }),
    /** copies into `dir` ("name copy.ext" when taken); the new path */
    copy: (from: string, dir: string): Promise<string> => invoke('fs_copy', { from, dir }),
    /** to the Recycle Bin */
    trash: (path: string): Promise<void> => invoke('fs_trash', { path }),
    create: (path: string, dir: boolean): Promise<void> => invoke('fs_create', { path, dir }),
    onChanged: (cb: (paths: string[]) => void): Off => on('fs:changed', cb),
    /** the repository moved: a commit, checkout, pull, staging (or events were lost) */
    onGitChanged: (cb: () => void): Off => on('git:changed', cb)
  },
  git: {
    state: (folder: string): Promise<GitState> => invoke('git_state', { folder }),
    original: (root: string, rel: string): Promise<string> => invoke('git_original', { root, rel }),
    init: (folder: string): Promise<void> => invoke('git_init', { folder }),
    show: (root: string, rev: string, rel: string): Promise<string> => invoke('git_show', { root, rev, rel }),
    log: (root: string): Promise<GitCommit[]> => invoke('git_log', { root }),
    commitFiles: (root: string, hash: string): Promise<GitCommitFile[]> => invoke('git_commit_files', { root, hash }),
    branches: (root: string): Promise<GitBranch[]> => invoke('git_branches', { root }),
    checkout: (root: string, name: string, remote: boolean): Promise<void> => invoke('git_checkout', { root, name, remote }),
    /** back to HEAD; new / untracked files are removed */
    discard: (root: string, rel: string, status: string): Promise<void> => invoke('git_discard', { root, rel, status })
  },
  app: {
    settings: (): Promise<AppSettings> => invoke('app_settings'),
    rememberFolder: (folder: string): Promise<AppSettings> => invoke('app_remember_folder', { folder }),
    setSettings: (patch: Partial<AppSettings>): Promise<AppSettings> => invoke('app_set_settings', { patch }),
    pickFolder: (): Promise<string | null> => invoke('app_pick_folder'),
    pickFiles: (defaultPath?: string): Promise<string[]> => invoke('app_pick_files', { defaultPath }),
    fileInfo: (paths: string[]): Promise<{ path: string; size: number; dir: boolean; preview?: string }[]> => invoke('app_file_info', { paths }),
    openExternal: (url: string): Promise<void> => invoke('app_open_external', { url }),
    clearBrowserCache: (): Promise<void> => invoke('app_clear_browser_cache'),
    relaunch: (): Promise<boolean> => invoke('app_relaunch'),
    /** the newer version published on GitHub, null when up to date */
    checkUpdate: (): Promise<string | null> => invoke('app_check_update'),
    /** downloads and runs the newest installer; the app quits and the installer restarts it */
    installUpdate: (): Promise<void> => invoke('app_install_update'),
    /** opens the screen-capture editor (Windows); `hide` takes the app off the screen first */
    capture: (hide: boolean): Promise<void> => invoke('capture_start', { hide }),
    /** a capture was sent to chat: the saved PNG's path (it is on the clipboard too) */
    onCaptured: (cb: (path: string) => void): Off => on('capture:done', cb),
    saveImage: (data: Uint8Array, ext: string): Promise<string> => invoke('app_save_image', data, { headers: { 'x-ext': ext } }),
    /** paths of files copied in Explorer (Ctrl+C on a file), [] when the clipboard holds none */
    clipboardFiles: (): Promise<string[]> => invoke('app_clipboard_files'),
    /**
     * A WebView never learns where a dropped file lives on disk; the window does. Files dragged in
     * from Explorer arrive here with their paths and the drop point, instead of as DOM drop events.
     */
    onFileDrop: (cb: (e: DropEvent) => void): Off => {
      let off: UnlistenFn | null = null
      let dead = false
      let last: string[] = []
      void getCurrentWebview()
        .onDragDropEvent(({ payload: p }) => {
          const ratio = window.devicePixelRatio || 1
          const pos = 'position' in p ? { x: p.position.x / ratio, y: p.position.y / ratio } : { x: -1, y: -1 }
          if (p.type === 'enter') last = p.paths
          const paths = p.type === 'drop' ? p.paths : last
          cb({ kind: p.type, paths, ...pos })
        })
        .then((u) => (dead ? u() : (off = u)))
      return () => {
        dead = true
        off?.()
      }
    }
  }
}

export type Api = typeof api

declare global {
  interface Window {
    api: Api
  }
}

window.api = api

/* ---------------- file drops → the element under the pointer ---------------- */

/** What a `vibe-drop` DOM event carries: `drop` has the dropped files' paths. */
export interface VibeDrop {
  kind: 'enter' | 'leave' | 'drop'
  paths: string[]
}

/**
 * The window receives drops from Explorer (with real paths); this routes them to the element under
 * the pointer as a bubbling `vibe-drop` CustomEvent, with enter/leave as the pointer moves.
 */
let hovered: Element | null = null
const fire = (el: Element | null, detail: VibeDrop): void => {
  el?.dispatchEvent(new CustomEvent<VibeDrop>('vibe-drop', { bubbles: true, detail }))
}
api.app.onFileDrop((e) => {
  if (e.kind === 'leave') {
    fire(hovered, { kind: 'leave', paths: e.paths })
    hovered = null
    return
  }
  const el = document.elementFromPoint(e.x, e.y)
  if (el !== hovered) {
    fire(hovered, { kind: 'leave', paths: e.paths })
    hovered = el
    fire(el, { kind: 'enter', paths: e.paths })
  }
  if (e.kind === 'drop') {
    fire(el, { kind: 'drop', paths: e.paths })
    fire(el, { kind: 'leave', paths: e.paths })
    hovered = null
  }
})
