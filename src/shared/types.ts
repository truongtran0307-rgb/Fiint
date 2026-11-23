import type { AgentId } from './agents'

export interface DirEntry {
  name: string
  path: string
  isDir: boolean
}

export type GitFileStatus = 'M' | 'A' | 'D' | 'R' | 'U' | '?' | 'C'

export interface GitFile {
  /** path relative to repo root, forward slashes */
  rel: string
  /** absolute path on disk */
  abs: string
  status: GitFileStatus
  staged: boolean
  added: number
  deleted: number
}

export interface GitState {
  isRepo: boolean
  root: string | null
  branch: string | null
  files: GitFile[]
}

export interface GitCommit {
  hash: string
  short: string
  author: string
  /** unix ms */
  date: number
  subject: string
}

export interface GitBranch {
  name: string
  current: boolean
  /** only on the remote (e.g. `origin/feature`): checking it out makes a local tracking branch */
  remote: boolean
}

export interface GitCommitFile {
  rel: string
  status: GitFileStatus
  added: number
  deleted: number
}

export interface PtySpawnOptions {
  id: string
  cwd: string
  kind: 'claude' | 'shell'
  /** which coding agent a chat terminal runs — default Claude Code */
  agent?: AgentId
  /** extra args for claude (or the chat's agent), e.g. ['--continue'] or ['--resume', id] */
  claudeArgs?: string[]
  cols: number
  rows: number
}

export interface SessionInfo {
  id: string
  title: string
  /** folder the session ran in — `claude --resume` must run from here */
  cwd: string
  /** repo the session belongs to (worktree sessions roll up to their repo) */
  projectCwd: string
  created: number
  updated: number
  cwdExists: boolean
}

export interface ProjectGroup {
  key: string
  cwd: string
  name: string
  updated: number
  sessions: SessionInfo[]
}

export type WindowMode = 'window' | 'dropdown'
export type ThemePref = 'system' | 'light' | 'dark' | 'blue'

export interface AppSettings {
  recentFolders: string[]
  lastFolder: string | null
  /** 'dropdown' = Guake-style: slides from the top edge on a global hotkey */
  mode: WindowMode
  hotkey: string
  /** drop-down height as a fraction of the screen work area */
  dropHeight: number
  dropOpacity: number
  hideOnBlur: boolean
  theme: ThemePref
  /** defaults for new chats: '' = Claude Code's own default */
  model: string
  effort: string
  /** default | acceptEdits | plan | bypassPermissions — '' = Claude Code's own default */
  permissionMode: string
  /** Linux-style overlay: transparent window background, text stays solid (needs a relaunch) */
  seeThrough: boolean
  /** code editor (Monaco): font family name, size in px, ligatures */
  editorFont: string
  editorFontSize: number
  editorLigatures: boolean
  /** start (hidden in the tray) when Windows signs in — undefined = on */
  openAtLogin?: boolean
}

export interface WindowState {
  mode: WindowMode
  maximized: boolean
  hotkey: string
  hotkeyOk: boolean
}

/** One plan limit from Claude's usage API (what /usage and the Claude app show). */
export interface UsageLimit {
  /** session = 5-hour window, weekly_all = all models this week, weekly_scoped = one model this week */
  kind: string
  label: string
  percent: number
  severity: string
  resetsAt: string | null
}

export interface AgentInfo {
  id: AgentId
  found: boolean
}

export interface ClaudeInfo {
  found: boolean
  path: string | null
  shell: string
}

/* ---------------- SQL Server (Database tab + Claude's mssql tools) ---------------- */

/** A saved connection as the UI sees it — no password. */
export interface DbConnection {
  id: string
  name: string
  server: string
  database: string
  auth: 'sql' | 'windows'
  user: string
  /** Claude's queries run in a transaction that is always rolled back */
  claudeReadOnly: boolean
  color?: string
}

export interface DbConnectionInput {
  id?: string
  name: string
  conn: string
  claudeReadOnly: boolean
  color?: string
}

/** A connection string found in the project or in SSMS, ready to add. */
export interface DbFound {
  name: string
  conn: string
  /** where it came from: a project file, or "SSMS 20.0" */
  file: string
  /** SSMS knew the server and login but had no saved password */
  needsPassword?: boolean
}

export interface DbObject {
  schema: string
  name: string
  /** U table, V view, P procedure, FN / IF / TF function */
  type: 'U' | 'V' | 'P' | 'FN' | 'IF' | 'TF'
}

export interface DbColumn {
  name: string
  type: string
  nullable: boolean
  identity: boolean
  pk: boolean
  /** foreign key target: schema.table.column */
  ref?: string
}

export interface DbResultSet {
  columns: { name: string; type: string }[]
  rows: unknown[][]
  truncated: boolean
}

export interface DbQueryResult {
  ok: boolean
  error?: string
  results: DbResultSet[]
  messages: string[]
  /** rows inserted / updated / deleted; -1 for none */
  affected: number
  ms: number
}

/* ---------------- Zalo (Zalo tab + Claude's zalo tools) ---------------- */

export interface ZaloState {
  status: 'off' | 'qr' | 'connecting' | 'online' | 'error'
  /** QR code to scan (data: URL) while status is 'qr' */
  qr?: string
  /** name shown on the phone that scanned the QR, waiting for "Đăng nhập" there */
  scanned?: string
  error?: string
  self?: { id: string; name: string; avatar: string }
}

/** Web tab bookmark: a link, or a folder of them */
export interface WebBookmark {
  title: string
  url?: string
  children?: WebBookmark[]
}

export interface WebData {
  /** when bookmarks and history were imported from Chrome (0 = not yet) */
  importedAt: number
  bookmarks: WebBookmark[]
  /** Chrome is installed, so there is something to import */
  chromeFound: boolean
}

export interface WebSuggestion {
  url: string
  title: string
  bookmark?: boolean
}

/** A background task Claude started in a chat */
export interface ChatTask {
  id: string
  command: string
  description: string
  /** running, completed, failed, killed… (as Claude Code reports it) */
  status: string
  startedAt: number
  endedAt?: number
  outputFile?: string
  /** the end of its output */
  output?: string
}

/** A file Claude created, edited or read in a chat (its Write / Edit / Read tools) */
export interface ChatFile {
  path: string
  /** a file read and then changed counts as changed */
  action: 'created' | 'edited' | 'read'
  /** successful Write / Edit calls on it */
  edits: number
  /** the last time Claude touched it */
  at: number
  exists: boolean
}

/** One item of the Zalo "Việc của tôi" checklist, summarised by Claude from messages sent to me. */
export interface ZaloTask {
  id: string
  title: string
  detail?: string
  threadId: string
  threadName: string
  group: boolean
  /** the message that asked for it */
  msgId?: string
  from: string
  /** Zalo user id of who asked */
  fromId?: string
  /** their avatar url (filled in when sent to the UI) */
  avatar?: string
  ts: number
  /** todo: not answered · doing: answered, not finished · done: reported done / resolved */
  status: 'todo' | 'doing' | 'done'
  /** status set by hand — Claude won't move it back */
  manual?: boolean
  updatedAt: number
}

/** A message to me that just arrived and is being turned into a task. */
export interface ZaloIncoming {
  threadId: string
  msgId: string
  threadName: string
  group: boolean
  from: string
  avatar?: string
  text: string
  ts: number
}

export interface ZaloTasksState {
  tasks: ZaloTask[]
  running: boolean
  analyzedAt: number
  error?: string
  /** new messages to me waiting for / in the current analysis */
  incoming?: ZaloIncoming[]
  /** ids of tasks the last analysis created (highlighted in the list) */
  fresh?: string[]
  /** ids of tasks whose status the last analysis changed */
  changed?: string[]
  /** tasks created / changed by Claude that I haven't looked at yet → the badge on the Zalo tab */
  unseen?: Record<string, 'new' | 'updated'>
  /** a batch of incoming messages is waiting: goes to Claude at this time unless more arrive */
  dueAt?: number
}
export interface ChatMessage {
  id: string
  role: 'user' | 'assistant'
  text: string
  at: number
}


/** Bảng việc cho iPhone: link Wi-Fi do máy Mac phát (Mac giữ phiên Zalo duy nhất). */
export interface ZaloPhoneInfo {
  url?: string | null
  /** mã QR của link, dạng SVG */
  qr?: string | null
  error?: string | null
}
