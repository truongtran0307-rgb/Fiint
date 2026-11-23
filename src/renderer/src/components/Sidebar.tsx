import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Collapse, SlidePill } from './motion'
import {
  ChevronRight,
  FolderGit2,
  FolderPlus,
  RefreshCw,
  RotateCcw,
  X,
  ChevronsDownUp,
  Copy,
  FolderOpen,
  Link2,
  Search,
  MessageSquare,
  Pencil,
  Plus,
  FilePlus,
  FileText,
  Scissors,
  ClipboardPaste,
  CopyPlus,
  Trash2
} from 'lucide-react'
import { basename, useStore, type Chat, type EditorTab } from '../store'
import {
  closeChat,
  closeTab,
  defaultAgent,
  loadSessions,
  openFolder,
  newChatIn,
  openFile,
  openSession,
  refreshGit,
  renameChat,
  restartChat,
  selectChat
} from '../actions'
import { hasUnsaved } from '../fileModelsLite'
import { RenameInput, startRename, useRenaming } from './InlineRename'
import { openMenu } from './ContextMenu'
import { FileIcon, FolderIcon, StatusDot } from './icons'
import type { DirEntry, GitFile, ProjectGroup, SessionInfo } from '../../../shared/types'
import { agentDef } from '../../../shared/agents'
import { NewChatMenu } from './AgentMenu'
import { ZaloSideGroup } from './zalo/ZaloSide'

export function Sidebar(): React.JSX.Element {
  // files and git changes live in the Source code tab; the sidebar is just the chats
  return (
    <div className="sidebar">
      <ChatsView />
    </div>
  )
}

function SectionHeader({ title, children }: { title: string; children?: React.ReactNode }): React.JSX.Element {
  return (
    <div className="section-header">
      <span>{title}</span>
      <div className="section-actions">{children}</div>
    </div>
  )
}

/* ================================ Chats ================================ */

function useTick(ms: number): void {
  const [, force] = useState(0)
  useEffect(() => {
    const t = setInterval(() => force((n) => n + 1), ms)
    return () => clearInterval(t)
  }, [ms])
}

const normKey = (p: string): string => p.toLowerCase().replace(/[\\/]+$/, '')

function ChatsView(): React.JSX.Element {
  useTick(30000)
  // New chat lives on the + of each project (and Ctrl+Shift+N), so the list starts right away
  const disabled = !useStore((s) => !!s.claude?.found || s.agents.some((a) => a.found))
  return (
    <>
      <div className="scroll">
        <ProjectsTree />
      </div>
      <div className="sidebar-foot">
        <button
          className="btn btn-primary btn-block"
          disabled={disabled}
          title="Chọn (hoặc tạo) một thư mục rồi bắt đầu chat trong đó"
          onClick={() => openFolder(null, { autoChat: true })}
        >
          <FolderPlus size={15} /> Dự án mới
        </button>
      </div>
    </>
  )
}

const EXPANDED_KEY = 'tinjs.expandedProjects'
function loadExpanded(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(EXPANDED_KEY) ?? '[]'))
  } catch {
    return new Set()
  }
}

const PAGE = 8

const RECENT_MAX = 5

interface RecentItem {
  key: string
  title: string
  project: string
  updated: number
  /** open in this window */
  chat?: Chat
  /** in Claude's history (resumable) */
  session?: SessionInfo
}

/** A row of Recent; plain props, so it skips re-rendering while nothing about it changed. */
const RecentRow = memo(function RecentRow(props: {
  title: string
  project: string
  updated: number
  chatId?: string
  status?: Chat['status']
  attention?: boolean
  session?: SessionInfo
  active: boolean
}): React.JSX.Element {
  const { title, project, updated, chatId, status, attention, session, active } = props
  return (
    <div
      className={`sess-row${active ? ' active' : ''}`}
      onClick={() => (chatId ? selectChat(chatId) : session && void openSession(session))}
      onContextMenu={(e) => chatMenu(e, { chatId, session }, 'recent')}
      title={`${title}\n${project} · ${new Date(updated).toLocaleString()}`}
    >
      {status ? <StatusDot status={status} attention={attention} /> : <span className="sess-bullet" />}
      <ChatTitle place="recent" chatId={chatId} title={title} />
      <span className="sess-time">{shortAgo(updated)}</span>
    </div>
  )
})

/** The newest chats of every project: open ones not in the history yet, then the history. */
function recentItems(projects: ProjectGroup[], chats: Chat[]): RecentItem[] {
  const live = new Map(chats.filter((c) => c.sessionId).map((c) => [c.sessionId!, c]))
  const history: RecentItem[] = projects.flatMap((g) =>
    g.sessions.map((s) => {
      const chat = live.get(s.id)
      return { key: s.id, title: chat?.pinnedTitle ? chat.title : s.title || chat?.title || 'Chat', project: g.name, updated: s.updated, chat, session: s }
    })
  )
  const known = new Set(history.map((r) => r.key))
  const fresh: RecentItem[] = chats
    .filter((c) => !c.sessionId || !known.has(c.sessionId))
    .map((c) => ({ key: c.id, title: c.title, project: basename(c.cwd), updated: c.createdAt, chat: c }))
  return [...fresh, ...history].sort((a, b) => b.updated - a.updated).slice(0, RECENT_MAX)
}

/** Every project + conversation from Claude Code's store — the same list the Claude app shows. */
function ProjectsTree(): React.JSX.Element {
  const projects = useStore((s) => s.projects)
  const root = useStore((s) => s.root)
  const chats = useStore((s) => s.chats)
  const activeChatId = useStore((s) => s.activeChatId)
  const [expanded, setExpanded] = useState<Set<string>>(loadExpanded)
  const [more, setMore] = useState<Set<string>>(new Set())
  const [q, setQ] = useState('')

  // the project you're in starts open
  useEffect(() => {
    if (!root) return
    const key = normKey(root)
    setExpanded((e) => (e.has(key) ? e : new Set(e).add(key)))
  }, [root])

  useEffect(() => {
    try {
      localStorage.setItem(EXPANDED_KEY, JSON.stringify([...expanded]))
    } catch {
      /* storage unavailable */
    }
  }, [expanded])

  const liveBySession = useMemo(() => new Map(chats.filter((c) => c.sessionId).map((c) => [c.sessionId!, c])), [chats])
  const activeSession = chats.find((c) => c.id === activeChatId)?.sessionId

  // Open chats the session store doesn't list (yet) go under their project, adding the project when
  // it has no Claude history: Codex / Grok / Gemini… chats, and new Claude chats until Claude writes
  // their transcript (only after the first message; a missed file event could delay it further).
  const slashKey = (p: string): string => normKey(p).replace(/\//g, '\\')
  const known = useMemo(() => new Set(projects.flatMap((g) => g.sessions.map((s) => s.id))), [projects])
  const agentChats = chats.filter((c) => agentDef(c.agent).id !== 'claude' || (!!c.sessionId && !known.has(c.sessionId)))
  const allProjects: ProjectGroup[] = [...projects]
  for (const c of agentChats) {
    if (allProjects.some((g) => slashKey(g.key) === slashKey(c.cwd))) continue
    allProjects.unshift({ key: normKey(c.cwd), cwd: c.cwd, name: basename(c.cwd), updated: c.createdAt, sessions: [] })
  }
  const needle = q.trim().toLowerCase()
  // Opening a chat touches its transcript (resume, focus…), which would jump it to the top under
  // the cursor. Rows already listed keep their place; only chats newly entering the top 5 go first.
  // A row is known by its session id and, once open, its chat id too: resuming a session or a new
  // chat getting its session id must not count as a new entry.
  const recentOrder = useRef(new Map<string, number>())
  const recent = useMemo(() => {
    const prev = recentOrder.current
    const ids = (it: RecentItem): string[] => [it.key, it.session?.id, it.chat?.id, it.chat?.sessionId].filter((x): x is string => !!x)
    const pos = (it: RecentItem): number | undefined => ids(it).map((k) => prev.get(k)).find((p) => p !== undefined)
    const top = recentItems(projects, chats)
    const kept = top.filter((it) => pos(it) !== undefined).sort((a, b) => pos(a)! - pos(b)!)
    const rows = [...top.filter((it) => pos(it) === undefined), ...kept]
    recentOrder.current = new Map(rows.flatMap((it, i) => ids(it).map((k) => [k, i] as const)))
    return rows
  }, [projects, chats])
  const agentRows = (g: ProjectGroup): typeof agentChats =>
    agentChats.filter(
      (c) => slashKey(c.cwd) === slashKey(g.key) && (!needle || g.name.toLowerCase().includes(needle) || c.title.toLowerCase().includes(needle))
    )
  const visible = needle
    ? allProjects
        .map((g) => ({
          ...g,
          sessions: g.name.toLowerCase().includes(needle) ? g.sessions : g.sessions.filter((s) => s.title.toLowerCase().includes(needle))
        }))
        .filter((g) => g.sessions.length > 0 || agentRows(g).length > 0)
    : allProjects

  const flip = (set: (f: (s: Set<string>) => Set<string>) => void, key: string): void =>
    set((e) => {
      const n = new Set(e)
      if (n.has(key)) n.delete(key)
      else n.add(key)
      return n
    })

  return (
    <div className="side-group">
      <SlidePill selector=".sess-row.active" className="sess-pill" />
      <div className="side-group-label">
        Dự án
        <span className="sync-pill" title="Đọc trực tiếp từ ~/.claude/projects — cùng nơi lưu với app Claude">
          <Link2 size={10} /> Claude
        </span>
        <button className="icon-btn tiny" title="Làm mới" onClick={() => loadSessions()}>
          <RefreshCw size={12} />
        </button>
      </div>
      <div className="tree-filter">
        <Search size={12} />
        <input placeholder="Lọc chat…" value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === 'Escape' && setQ('')} />
      </div>

      {/* the 5 newest chats across every project */}
      {!needle && recent.length > 0 && (
        <div className="proj recent-group">
          <div className="proj-row recent-head">
            <span className="proj-name">Gần đây</span>
          </div>
          {recent.map((it) => (
            <RecentRow
              key={it.key}
              title={it.title}
              project={it.project}
              updated={it.updated}
              chatId={it.chat?.id}
              status={it.chat?.status}
              attention={it.chat?.attention}
              session={it.session}
              active={!!it.chat && it.chat.id === activeChatId}
            />
          ))}
        </div>
      )}
      <ZaloSideGroup filter={q} />
      <div className="agent-groups-sep" />

      {visible.length === 0 && <div className="tree-empty">{projects.length ? 'Không có chat nào khớp' : 'Chưa có lịch sử Claude Code.'}</div>}

      {visible.map((g) => {
        const open = needle ? true : expanded.has(g.key)
        const isRoot = !!root && normKey(root) === g.key
        const others = agentRows(g)
        const liveCount = g.sessions.filter((s) => liveBySession.has(s.id)).length + others.length
        const shown = needle || more.has(g.key) ? g.sessions : g.sessions.slice(0, PAGE)
        return (
          <div key={g.key} className="proj">
            <div className={`proj-row${isRoot ? ' current' : ''}`} onClick={() => flip(setExpanded, g.key)} onContextMenu={(e) => projectMenu(e, g)} title={g.cwd}>
              <span className="proj-name">{g.name}</span>
              <ChevronRight size={13} className={`proj-chev${open ? ' open' : ''}`} />
              {liveCount > 0 && <span className="proj-live" title={`${liveCount} đang mở`} />}
              <span className="proj-spacer" />
              <NewChatMenu
                className="icon-btn tiny proj-add"
                size={14}
                title={`Chat mới trong ${g.name} — chuột phải để chọn AI khác`}
                onPick={(agent) => newChatIn(g.cwd, agent)}
              />
            </div>
            <Collapse open={open}>
            {others.map((c) => (
              <div
                key={c.id}
                className={`sess-row${c.id === activeChatId ? ' active' : ''}`}
                onClick={() => selectChat(c.id)}
                onContextMenu={(e) => chatMenu(e, { chatId: c.id }, 'tree')}
                title={`${c.title}\n${agentDef(c.agent).label} · ${c.cwd}`}
              >
                <StatusDot status={c.status} attention={c.attention} />
                <ChatTitle place="tree" chatId={c.id} title={c.title} />
                <span className="sess-time">{agentDef(c.agent).id === 'claude' ? shortAgo(c.createdAt) : agentDef(c.agent).label}</span>
              </div>
            ))}
            {shown.map((s) => {
              const live = liveBySession.get(s.id)
              return <SessRow key={s.id} s={s} active={activeSession === s.id} status={live?.status} attention={live?.attention} />
            })}
            {!needle && g.sessions.length > PAGE && (
              <button className="sess-more" onClick={() => flip(setMore, g.key)}>
                {more.has(g.key) ? 'Thu gọn' : `Xem thêm ${g.sessions.length - PAGE}`}
              </button>
            )}
            </Collapse>
          </div>
        )
      })}
    </div>
  )
}

/**
 * One conversation under its project. Memoized: the tree re-renders on every sessions ping while a
 * chat works; rows whose session object loadSessions kept (unchanged) skip it.
 */
const SessRow = memo(function SessRow(props: { s: SessionInfo; active: boolean; status?: Chat['status']; attention?: boolean }): React.JSX.Element {
  const { s, active, status, attention } = props
  // primitives only: this row re-renders when they change, not on every chat update
  const liveId = useStore((st) => st.chats.find((c) => c.sessionId === s.id)?.id)
  const pinned = useStore((st) => {
    const c = st.chats.find((x) => x.sessionId === s.id)
    return c?.pinnedTitle ? c.title : undefined
  })
  return (
    <div
      className={`sess-row${active ? ' active' : ''}${s.cwdExists ? '' : ' gone'}`}
      onClick={() => openSession(s)}
      onContextMenu={(e) => chatMenu(e, { session: s }, 'tree')}
      title={`${s.title}\n${s.cwd}\nCập nhật ${new Date(s.updated).toLocaleString()}`}
    >
      {status ? <StatusDot status={status} attention={attention} /> : <span className="sess-bullet" />}
      <ChatTitle place="tree" chatId={liveId} title={pinned ?? s.title} />
      <span className="sess-time">{shortAgo(s.updated)}</span>
    </div>
  )
})

/** A chat's name in a row; a text box while it's renamed from that row's menu. */
export function ChatTitle({ place, chatId, title, className = 'sess-title' }: { place: string; chatId?: string; title: string; className?: string }): React.JSX.Element {
  const editing = useRenaming((r) => !!chatId && r.key === `chat:${place}:${chatId}`)
  if (editing) return <RenameInput value={title} onCommit={(t) => renameChat(chatId!, t)} />
  return <span className={className}>{title}</span>
}

/**
 * Right-click on a chat: an open chat (`chatId`) and/or a session from the history. `place` names
 * the row it was opened on (the same chat is listed in several), so Rename edits that one.
 */
export function chatMenu(e: React.MouseEvent, ref: { chatId?: string; session?: SessionInfo }, place: string): void {
  const chats = useStore.getState().chats
  const chat = chats.find((c) => c.id === ref.chatId) ?? (ref.session ? chats.find((c) => c.sessionId === ref.session!.id) : undefined)
  const s = ref.session
  const title = chat?.title ?? s?.title ?? 'Chat'
  const cwd = chat?.cwd ?? s?.cwd
  const sessionId = chat?.sessionId ?? s?.id
  openMenu(e, [
    { label: 'Mở', icon: <MessageSquare size={14} />, action: () => (chat ? selectChat(chat.id) : s && openSession(s)) },
    chat && {
      label: 'Đổi tên…',
      icon: <Pencil size={14} />,
      action: () => startRename(`chat:${place}:${chat.id}`)
    },
    chat?.status === 'exited' && { label: 'Khởi động lại', icon: <RotateCcw size={14} />, action: () => restartChat(chat.id) },
    chat && { label: 'Đóng chat', icon: <X size={14} />, action: () => closeChat(chat.id) },
    'sep',
    { label: 'Sao chép tiêu đề', icon: <Copy size={14} />, action: () => navigator.clipboard.writeText(title) },
    !!sessionId && { label: 'Sao chép mã phiên', icon: <Copy size={14} />, action: () => navigator.clipboard.writeText(sessionId) },
    !!cwd && { label: 'Mở thư mục trong File Explorer', icon: <FolderOpen size={14} />, action: () => window.api.fs.openPath(cwd) },
    'sep',
    {
      label: 'Xoá chat…',
      icon: <Trash2 size={14} />,
      hint: 'vào Thùng rác',
      action: async () => {
        const working = chat?.status === 'working' ? `\n\n${agentDef(chat.agent).label} vẫn đang làm — sẽ bị dừng lại.` : ''
        if (!window.confirm(`Xoá chat "${title}"?\nLịch sử chat sẽ vào Thùng rác.${working}`)) return
        if (chat) {
          closeChat(chat.id)
          // let the process exit first, or its last write would bring the transcript back
          await new Promise((r) => setTimeout(r, 800))
        }
        // a chat that never sent a message has no saved history
        const saved = !!sessionId && useStore.getState().projects.some((g) => g.sessions.some((x) => x.id === sessionId))
        if (saved) await window.api.sessions.remove(sessionId).catch((err) => window.alert(String(err)))
        loadSessions()
      }
    }
  ])
}

/** Right-click on a project in the sidebar. */
function projectMenu(e: React.MouseEvent, g: ProjectGroup): void {
  const agent = defaultAgent()
  openMenu(e, [
    !!agent && { label: 'Chat mới', icon: <Plus size={14} />, action: () => newChatIn(g.cwd, agent) },
    { label: 'Mở làm không gian làm việc', icon: <FolderGit2 size={14} />, action: () => openFolder(g.cwd, { autoChat: false }) },
    'sep',
    { label: 'Mở thư mục trong File Explorer', icon: <FolderOpen size={14} />, action: () => window.api.fs.openPath(g.cwd) },
    { label: 'Sao chép đường dẫn', icon: <Copy size={14} />, action: () => navigator.clipboard.writeText(g.cwd) }
  ])
}

/** "now", "5m", "3h", "2d", then the date. */
export function shortAgo(t: number): string {
  const m = Math.floor((Date.now() - t) / 60000)
  if (m < 1) return 'vừa xong'
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h`
  const d = Math.floor(h / 24)
  if (d < 30) return `${d}d`
  return new Date(t).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
}

/* ================================ Explorer ================================ */

function useGitDecorations(): Map<string, GitFile['status']> {
  // only paths + statuses matter here; +/- counts change on every edit Claude makes, and a new map
  // re-rendered the whole tree each time
  const sig = useStore((s) => s.git.files.map((f) => f.status + f.abs).join('\n'))
  return useMemo(() => {
    const m = new Map<string, GitFile['status']>()
    const files = useStore.getState().git.files
    for (const f of files) {
      const abs = f.abs.replace(/\\/g, '/').toLowerCase()
      m.set(abs, f.status)
      // bubble up to parent folders
      let p = abs
      while (p.includes('/')) {
        p = p.slice(0, p.lastIndexOf('/'))
        if (!m.has(p)) m.set(p, 'M')
      }
    }
    return m
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig])
}

export function ExplorerView(): React.JSX.Element | null {
  const root = useStore((s) => s.root)
  const [collapseKey, setCollapseKey] = useState(0)
  const deco = useGitDecorations()
  if (!root) return null
  return <ExplorerTree root={root} collapseKey={collapseKey} setCollapseKey={setCollapseKey} deco={deco} />
}

type OpenCtx = (e: React.MouseEvent, path: string, isDir: boolean) => void

function ExplorerTree(props: { root: string; collapseKey: number; setCollapseKey: (f: (k: number) => number) => void; deco: Map<string, string> }): React.JSX.Element {
  const { root, collapseKey, setCollapseKey, deco } = props
  // stable, so the memoized tree nodes don't all re-render with this one
  const openCtx = useCallback<OpenCtx>((e, path, isDir) => explorerMenu(e, path, isDir, root), [root])
  return (
    <>
      <SectionHeader title="Tệp">
        <button className="icon-btn" title="Tệp mới" onClick={() => createIn(root, false)}>
          <FilePlus size={14} />
        </button>
        <button className="icon-btn" title="Thư mục mới" onClick={() => createIn(root, true)}>
          <FolderPlus size={14} />
        </button>
        <button className="icon-btn" title="Làm mới" onClick={() => useStore.setState((s) => ({ fsTick: s.fsTick + 1 }))}>
          <RefreshCw size={14} />
        </button>
        <button className="icon-btn" title="Thu gọn tất cả" onClick={() => setCollapseKey((k) => k + 1)}>
          <ChevronsDownUp size={14} />
        </button>
      </SectionHeader>
      <div className="tree-root-label" onContextMenu={(e) => openCtx(e, root, true)}>
        {basename(root)}
      </div>
      {/* empty space below the files: the project folder's menu */}
      <div className="scroll tree" key={collapseKey} onContextMenu={(e) => openCtx(e, root, true)}>
        <DirChildren dir={root} depth={0} deco={deco} openCtx={openCtx} />
      </div>
    </>
  )
}

/** A file or folder copied / cut in the explorer, waiting for Paste. */
let fileClip: { path: string; cut: boolean } | null = null

const sepOf = (p: string): string => (p.includes('\\') ? '\\' : '/')
const dirOf = (p: string): string => p.slice(0, Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/')))
const refreshTree = (): void => {
  useStore.setState((s) => ({ fsTick: s.fsTick + 1 }))
  refreshGit()
}
/** Run a file operation; a failure shows its message instead of vanishing. */
const fsOp = (f: () => Promise<unknown>) => async (): Promise<void> => {
  try {
    await f()
  } catch (err) {
    window.alert(String(err))
  }
  refreshTree()
}

/** New file / folder in `dir`: an empty row opens there for its name (see NewEntryRow). */
const createIn = (dir: string, isDir: boolean): void => startRename(`new:${isDir ? 'dir' : 'file'}:${dir}`)

/** Create the file / folder named in a new row (files open in the editor). */
async function createNamed(dir: string, isDir: boolean, name: string): Promise<void> {
  const path = dir + sepOf(dir) + name
  await window.api.fs.create(path, isDir)
  if (!isDir) openFile(path)
}

/** The row a new file / folder's name is typed into, at the top of its folder. */
function NewEntryRow({ dir, isDir, depth }: { dir: string; isDir: boolean; depth: number }): React.JSX.Element {
  return (
    <div className="tree-row" style={{ paddingLeft: 10 + depth * 12 }}>
      <span className="chev-spacer" />
      {isDir ? <FolderIcon open={false} /> : <FilePlus size={14} />}
      <RenameInput value="" onCommit={(name) => fsOp(() => createNamed(dir, isDir, name))()} />
    </div>
  )
}

/** Editor tabs of `path`, or of files inside it when it's a folder. */
function tabsUnder(path: string): EditorTab[] {
  const lower = path.toLowerCase()
  return useStore.getState().tabs.filter((t) => t.kind === 'file' && (t.path.toLowerCase() === lower || t.path.toLowerCase().startsWith(lower + sepOf(path))))
}

/** Rename a file / folder in place; its open editor tabs follow it. */
async function renamePath(path: string, next: string): Promise<void> {
  const open = tabsUnder(path)
  if (open.some((t) => hasUnsaved(t.path)) && !window.confirm('Thay đổi chưa lưu sẽ mất. Vẫn đổi tên?')) return
  const to = dirOf(path) + sepOf(path) + next
  await window.api.fs.rename(path, to)
  for (const t of open) closeTab(t.id)
  for (const t of open) openFile(to + t.path.slice(path.length))
}

/** Right-click on a file or folder in the explorer. */
function explorerMenu(e: React.MouseEvent, path: string, isDir: boolean, root: string): void {
  const isRoot = path === root
  const rel = path.slice(root.length).replace(/^[\\/]+/, '').replace(/\\/g, '/') || '.'
  const name = basename(path)
  const into = isDir ? path : dirOf(path)
  openMenu(e, [
    !isDir && { label: 'Mở', icon: <FileText size={14} />, action: () => openFile(path) },
    { label: 'Tệp mới', icon: <FilePlus size={14} />, action: () => createIn(into, false) },
    { label: 'Thư mục mới', icon: <FolderPlus size={14} />, action: () => createIn(into, true) },
    'sep',
    !isRoot && { label: 'Cắt', icon: <Scissors size={14} />, action: () => void (fileClip = { path, cut: true }) },
    !isRoot && { label: 'Sao chép', icon: <Copy size={14} />, action: () => void (fileClip = { path, cut: false }) },
    {
      label: 'Dán',
      icon: <ClipboardPaste size={14} />,
      hint: fileClip ? basename(fileClip.path) : undefined,
      // what was copied here first, else files copied in Windows Explorer
      action: fsOp(async () => {
        if (fileClip?.cut) {
          const to = into + sepOf(into) + basename(fileClip.path)
          if (to.toLowerCase() !== fileClip.path.toLowerCase()) await window.api.fs.rename(fileClip.path, to)
          fileClip = null
        } else if (fileClip) await window.api.fs.copy(fileClip.path, into)
        else for (const p of await window.api.app.clipboardFiles()) await window.api.fs.copy(p, into)
      })
    },
    !isRoot && { label: 'Nhân bản', icon: <CopyPlus size={14} />, action: fsOp(() => window.api.fs.copy(path, dirOf(path))) },
    'sep',
    !isRoot && {
      label: 'Đổi tên…',
      icon: <Pencil size={14} />,
      action: () => startRename('file:' + path)
    },
    !isRoot && {
      label: 'Xoá',
      icon: <Trash2 size={14} />,
      hint: 'vào Thùng rác',
      action: fsOp(async () => {
        if (!window.confirm(`Chuyển ${isDir ? 'thư mục' : 'tệp'} "${name}" vào Thùng rác?`)) return
        await window.api.fs.trash(path)
        for (const t of tabsUnder(path)) closeTab(t.id)
      })
    },
    'sep',
    isDir
      ? { label: 'Mở thư mục trong File Explorer', icon: <FolderOpen size={14} />, action: () => window.api.fs.openPath(path) }
      : { label: 'Hiện trong File Explorer', icon: <FolderOpen size={14} />, action: () => window.api.fs.reveal(path) },
    { label: 'Sao chép đường dẫn', icon: <Copy size={14} />, action: () => navigator.clipboard.writeText(path) },
    { label: 'Sao chép đường dẫn tương đối', icon: <Copy size={14} />, action: () => navigator.clipboard.writeText(rel) }
  ])
}

const normDir = (p: string): string => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase()

/** Parent folder of `p`, normalized like normDir. */
const parentOf = (p: string): string => {
  const n = normDir(p)
  return n.slice(0, Math.max(0, n.lastIndexOf('/')))
}

/** Re-renders when a file event touches something directly inside `dir` (not on every change anywhere). */
function useDirChanges(dir: string): number {
  const [n, setN] = useState(0)
  useEffect(() => {
    const key = normDir(dir)
    let timer: ReturnType<typeof setTimeout> | undefined
    const onChange = (e: Event): void => {
      const paths = (e as CustomEvent<string[]>).detail
      if (!paths.some((p) => parentOf(p) === key)) return
      clearTimeout(timer)
      timer = setTimeout(() => setN((x) => x + 1), 100)
    }
    window.addEventListener('fs-changed', onChange)
    return () => {
      clearTimeout(timer)
      window.removeEventListener('fs-changed', onChange)
    }
  }, [dir])
  return n
}

const sameEntries = (a: DirEntry[] | null, b: DirEntry[]): boolean =>
  !!a && a.length === b.length && a.every((x, i) => x.path === b[i].path && x.isDir === b[i].isDir && x.name === b[i].name)

function DirChildren({ dir, depth, deco, openCtx }: { dir: string; depth: number; deco: Map<string, string>; openCtx: OpenCtx }): React.JSX.Element {
  const [items, setItems] = useState<DirEntry[] | null>(null)
  // the Refresh button bumps fsTick; file events only reach the folders they touch
  const tick = useStore((s) => s.fsTick)
  const changed = useDirChanges(dir)
  const creating = useRenaming((r) => (r.key === `new:file:${dir}` ? 'file' : r.key === `new:dir:${dir}` ? 'dir' : null))
  useEffect(() => {
    let live = true
    window.api.fs
      .readDir(dir)
      .then((r) => live && setItems((prev) => (sameEntries(prev, r) ? prev : r)))
      .catch(() => live && setItems([]))
    return () => {
      live = false
    }
  }, [dir, tick, changed])
  if (!items) return <div className="tree-loading" style={{ paddingLeft: 12 + depth * 12 }} />
  return (
    <>
      {creating && <NewEntryRow dir={dir} isDir={creating === 'dir'} depth={depth} />}
      {items.map((it) => (
        <TreeNode key={it.path} item={it} depth={depth} deco={deco} openCtx={openCtx} />
      ))}
    </>
  )
}

const TreeNode = memo(function TreeNode({ item, depth, deco, openCtx }: { item: DirEntry; depth: number; deco: Map<string, string>; openCtx: OpenCtx }): React.JSX.Element {
  const [open, setOpen] = useState(false)
  // a boolean: re-renders only when this row starts / stops being the open file
  const isActive = useStore((s) => s.activeTabId === 'f:' + item.path.toLowerCase())
  const status = deco.get(item.path.replace(/\\/g, '/').toLowerCase())
  const renaming = useRenaming((r) => r.key === 'file:' + item.path)
  // New file / folder inside it: open it so the name row shows
  const creatingIn = useRenaming((r) => r.key === 'new:file:' + item.path || r.key === 'new:dir:' + item.path)
  useEffect(() => {
    if (creatingIn) setOpen(true)
  }, [creatingIn])
  return (
    <>
      <div
        className={`tree-row${isActive ? ' active' : ''}${status ? ` git-${status === '?' ? 'U' : status}` : ''}`}
        style={{ paddingLeft: 10 + depth * 12 }}
        onClick={() => (item.isDir ? setOpen(!open) : openFile(item.path))}
        onContextMenu={(e) => openCtx(e, item.path, item.isDir)}
        title={item.path}
      >
        {item.isDir ? (
          <ChevronRight size={13} className={`chev${open ? ' open' : ''}`} />
        ) : (
          <span className="chev-spacer" />
        )}
        {item.isDir ? <FolderIcon open={open} /> : <FileIcon name={item.name} />}
        {renaming ? (
          <RenameInput value={item.name} stem={!item.isDir} onCommit={(next) => fsOp(() => renamePath(item.path, next))()} />
        ) : (
          <span className="tree-name">{item.name}</span>
        )}
        {status && !item.isDir && <span className="tree-status">{status === '?' ? 'U' : status}</span>}
        {status && item.isDir && <span className="tree-dot" />}
      </div>
      {item.isDir && open && <DirChildren dir={item.path} depth={depth + 1} deco={deco} openCtx={openCtx} />}
    </>
  )
})

