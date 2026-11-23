import { useEffect, useRef, useState, type FormEvent } from 'react'
import { Check, FileText, Image as ImageIcon, LayoutTemplate, ListChecks, MessageSquare, Copy, Shield, ShieldOff, History, Plus, RefreshCw, RotateCcw, Sparkles, FastForward, Paperclip, Square, X, ExternalLink, ArrowUp, Settings2, TerminalSquare } from 'lucide-react'
import { openMenu } from './ContextMenu'
import { TEMPLATES, insertTemplate } from '../templates'
import { useStore, type Chat } from '../store'
import { SlidePill, useEnterAnimation } from './motion'
import {
  closeChat,
  defaultAgent,
  detectClaude,
  newChat,
  newChatWith,
  restartChat,
  selectChat,
  setChatEffort,
  setChatModel,
  setChatPermission,
  folderKey,
  loadProjectBypass,
  toggleProjectBypass
} from '../actions'
import { attachTerm, detachTerm, fitTerm, focusTerm, submitChatDraft, syncTermRenderer, writeToTerm } from '../terminals'
import { Logo, StatusDot, statusLabel } from './icons'
import { ChatImages, useChatImages } from './ChatImages'
import { ChatTasks, useChatTasks } from './ChatTasks'
import { ChatFiles, useChatFiles } from './ChatFiles'
import { ChatTitle, chatMenu } from './Sidebar'
import { AttachmentTray } from './AttachmentTray'
import { attachBlob, pickAttachments } from '../attachments'
import { NewChatMenu } from './AgentMenu'
import { AGENTS, agentDef } from '../../../shared/agents'
import { ChatConversation } from './ChatConversation'
import './chat-simple.css'

export function TerminalView({ id, autoFocus = true, rendered = true }: { id: string; autoFocus?: boolean; rendered?: boolean }): React.JSX.Element {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = ref.current!
    attachTerm(id, el, autoFocus)
    // A fit that changes the column count re-wraps the whole scrollback: while a panel is dragged,
    // at most ~12 a second (the last size always gets one), not one every frame.
    let raf = 0
    let last = 0
    let timer: ReturnType<typeof setTimeout> | undefined
    const run = (): void => {
      last = performance.now()
      fitTerm(id)
    }
    const ro = new ResizeObserver(() => {
      clearTimeout(timer)
      cancelAnimationFrame(raf)
      const wait = 80 - (performance.now() - last)
      if (wait <= 0) raf = requestAnimationFrame(run)
      else timer = setTimeout(() => (raf = requestAnimationFrame(run)), wait)
    })
    ro.observe(el)
    return () => {
      ro.disconnect()
      cancelAnimationFrame(raf)
      clearTimeout(timer)
      detachTerm(id)
    }
  }, [id, autoFocus])
  useEffect(() => syncTermRenderer(id), [id, rendered])
  return <div className="term-view" ref={ref} onMouseDown={() => setTimeout(() => focusTerm(id), 0)} />
}

/* ================================ Claude panel ================================ */


export function ClaudePanel(): React.JSX.Element {
  // only the open chat: other chats' status / title changes don't re-render this panel
  const active = useStore((s) => s.chats.find((c) => c.id === s.activeChatId) ?? null)
  const activeId = active?.id ?? null
  const claude = useStore((s) => s.claude)
  // Codex / Gemini… alone is enough to chat; the install card is for when no agent is there at all
  const anyAgent = useStore((s) => !!s.claude?.found || s.agents.some((a) => a.found))
  const [tab, setTab] = useState<'chat' | 'console' | 'images' | 'files' | 'tasks'>('chat')
  const [advanced, setAdvanced] = useState(false)
  const images = useChatImages(advanced && tab === 'images' ? active?.sessionId : undefined)
  const tasks = useChatTasks(advanced && tab === 'tasks' ? active?.sessionId : undefined)
  const files = useChatFiles(advanced && tab === 'files' ? active?.sessionId : undefined)
  // another chat opens on its Chat tab
  useEffect(() => {
    setTab('chat')
    setAdvanced(false)
  }, [activeId])
  // the terminal stays under Images / Files / Tasks, so it must not keep the keyboard
  const leaveChat = (t: 'images' | 'files' | 'tasks'): void => {
    const el = document.activeElement
    if (el instanceof HTMLElement && termFrame.current?.contains(el)) el.blur()
    setTab(t)
  }
  // switching chats: the new conversation eases in
  const termFrame = useRef<HTMLDivElement>(null)
  useEnterAnimation(termFrame, activeId)
  const usesConversation = agentDef(active?.agent).id === 'claude'
  const showConsole = tab === 'console' || (tab === 'chat' && !usesConversation)
  const showConversation = tab === 'chat' && usesConversation
  const openConsole = (): void => {
    setAdvanced(true)
    setTab('console')
    if (activeId) setTimeout(() => focusTerm(activeId), 0)
  }

  // Chats are picked from the sidebar, so there is no tab strip here — just the open chat's title row.
  return (
    <div className="claude-panel simple-chat">
      {claude && !anyAgent && <InstallCard />}
      {anyAgent && !active && <NoChat />}

      {active && (
        <>
          <div className="chat-titlebar">
            <div className="chat-titlebar-main" onContextMenu={(e) => chatMenu(e, { chatId: active.id }, 'title')}>
              <StatusDot status={active.status} attention={active.attention} />
              <ChatTitle place="title" chatId={active.id} title={active.title} className="chat-titlebar-title" />
              {agentDef(active.agent).id !== 'claude' && <span className="chat-agent">{agentDef(active.agent).label}</span>}
              <span className={`chat-state s-${active.status}`}>{active.needsInput ? 'Cần xác nhận' : active.status === 'idle' ? 'Sẵn sàng' : statusLabel[active.status]}</span>
            </div>
            <div className="chat-titlebar-actions">
              {active.status === 'exited' && (
                <button className="btn btn-ghost btn-sm" onClick={() => restartChat(active.id)}>
                  <RefreshCw size={13} /> Khởi động lại
                </button>
              )}
              {advanced && agentDef(active.agent).id === 'claude' && <BypassProjectButton chat={active} />}
              <button type="button" className="icon-btn simple-chat-settings" aria-label="Tùy chọn chat" title="Tùy chọn chat" aria-pressed={advanced} onClick={() => {
                if (advanced) setTab('chat')
                setAdvanced(!advanced)
              }}><Settings2 size={16} /></button>
              <NewChatMenu title="Chat mới với AI mặc định (Ctrl+Shift+N) — chuột phải để chọn AI khác" onPick={newChatWith} />
              <button className="icon-btn" title="Đóng chat (vẫn còn trong lịch sử bên trái)" onClick={() => closeChat(active.id)}>
                <X size={15} />
              </button>
            </div>
          </div>
          {advanced && <div className="code-side-switch chat-tabs">
            <SlidePill />
            <button
              className={tab === 'chat' ? 'on' : ''}
              onClick={() => setTab('chat')}
            >
              <MessageSquare size={13} /> Hội thoại
            </button>
            {usesConversation && <button className={tab === 'console' ? 'on' : ''} onClick={openConsole}><TerminalSquare size={13} /> Chế độ gốc</button>}
            <button className={tab === 'images' ? 'on' : ''} onClick={() => leaveChat('images')}>
              <ImageIcon size={13} /> Ảnh
            </button>
            <button className={tab === 'files' ? 'on' : ''} title="Tệp Claude đã tạo, sửa hoặc đọc trong chat này" onClick={() => leaveChat('files')}>
              <FileText size={13} /> Tệp
            </button>
            <button className={tab === 'tasks' ? 'on' : ''} title="Việc chạy nền Claude bắt đầu trong chat này" onClick={() => leaveChat('tasks')}>
              <ListChecks size={13} /> Tác vụ
            </button>
          </div>}
          {/*
            The terminal stays mounted and laid out (only made invisible): display:none would reset
            xterm's scroll position to the top of the buffer and replay the enter animation on return.
            Images / Tasks cover it instead.
          */}
          <div className={`term-frame${showConversation ? ' conversation-frame' : ''}`} ref={termFrame}>
            <div className="term-layer" inert={!showConsole} aria-hidden={!showConsole} style={showConsole ? undefined : { visibility: 'hidden' }}>
              <TerminalView id={active.id} autoFocus={false} rendered={showConsole} />
            </div>
            {showConversation && <ChatConversation key={active.id} chat={active} onOpenConsole={openConsole} />}
            {showConsole && <StartSkeleton show={active.status === 'starting'} />}
            {tab !== 'chat' && tab !== 'console' && (
              <div className="chat-overlay">
                {tab === 'images' ? <ChatImages images={images} /> : tab === 'files' ? <ChatFiles files={files} cwd={active.cwd} /> : <ChatTasks tasks={tasks} />}
              </div>
            )}
          </div>
          {tab === 'chat' && <ChatComposer key={active.id} chat={active} onOpenConsole={openConsole} />}
          {tab === 'console' && <AttachmentTray chatId={active.id} />}
          {advanced && <ChatStatusBar chat={active} />}
          {advanced && agentDef(active.agent).id === 'claude' && <ContextBar chat={active} />}
        </>
      )}
    </div>
  )
}

function ChatComposer({ chat, onOpenConsole }: { chat: Chat; onOpenConsole: () => void }): React.JSX.Element {
  const [sending, setSending] = useState(false)
  const [error, setError] = useState('')
  const draft = chat.draft ?? ''
  const locked = sending || chat.status === 'exited'
  const canSend = !locked && chat.status === 'idle' && !chat.needsInput && Boolean(draft.trim())

  async function send(e: FormEvent<HTMLFormElement>): Promise<void> {
    e.preventDefault()
    if (!canSend) return
    setSending(true)
    setError('')
    const at = Date.now()
    try {
      await submitChatDraft(chat.id, draft)
      const state = useStore.getState()
      if (state.chats.find((item) => item.id === chat.id)?.draft === draft) state.patchChat(chat.id, { draft: '', sentDraft: { text: draft.trim(), at } })
    } catch (problem) {
      setError(problem instanceof Error ? problem.message : String(problem))
    } finally {
      setSending(false)
    }
  }

  return (
    <form className="simple-composer" onSubmit={(e) => void send(e)}>
      <label className="simple-composer-label" htmlFor={`chat-draft-${chat.id}`}>Bạn muốn AI làm gì?</label>
      <div className="simple-composer-box">
        <AttachmentTray chatId={chat.id} />
        <textarea id={`chat-draft-${chat.id}`} rows={2} value={draft} disabled={locked}
          placeholder="Ví dụ: Lên lịch chuẩn bị cho hội thảo thứ Sáu…"
          onChange={(e) => { setError(''); useStore.getState().patchChat(chat.id, { draft: e.target.value }) }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault()
              e.currentTarget.form?.requestSubmit()
            }
          }}
          onPaste={(e) => {
            const images = Array.from(e.clipboardData.files).filter((file) => file.type.startsWith('image/'))
            if (!images.length) return
            e.preventDefault()
            void Promise.all(images.map((image) => attachBlob(chat.id, image))).catch(() => setError('Chưa đính kèm được ảnh đã dán.'))
          }} />
        <div className="simple-composer-actions">
          <div>
            <button type="button" disabled={locked} title="Đính kèm tệp" onClick={() => void pickAttachments(chat.id, chat.cwd)}><Paperclip size={15} /> <span>Tệp</span></button>
            <button type="button" disabled={locked} onClick={(e) => {
              const r = e.currentTarget.getBoundingClientRect()
              openMenu({ clientX: r.left, clientY: r.top, preventDefault: () => {}, stopPropagation: () => {} },
                TEMPLATES.map((item) => ({ label: item.label, icon: item.icon, hint: item.hint, action: () => insertTemplate(item.text) })))
            }}><LayoutTemplate size={15} /> <span>Mẫu việc</span></button>
          </div>
          {chat.status === 'working' && !chat.needsInput
            ? <button type="button" className="simple-send" onClick={() => writeToTerm(chat.id, '\x1b')}><Square size={13} /> Dừng</button>
            : <button className="simple-send" disabled={!canSend} title="Gửi cho AI (Enter)"><ArrowUp size={16} /> Gửi</button>}
        </div>
      </div>
      {error && <p role="alert">{error} <button type="button" className="conversation-link" onClick={onOpenConsole}>Mở chế độ gốc</button></p>}
    </form>
  )
}

/** Until the agent draws its first screen: a shimmering outline of its banner and prompt, faded out once it has. */
function StartSkeleton({ show }: { show: boolean }): React.JSX.Element | null {
  const [mounted, setMounted] = useState(show)
  useEffect(() => {
    if (show) return setMounted(true)
    const t = setTimeout(() => setMounted(false), 260)
    return () => clearTimeout(t)
  }, [show])
  if (!mounted) return null
  return (
    <div className={`chat-skel${show ? '' : ' out'}`} aria-hidden>
      <div className="chat-skel-head">
        <span className="sk sk-logo" />
        <div className="chat-skel-lines">
          <span className="sk" style={{ width: '38%' }} />
          <span className="sk" style={{ width: '62%' }} />
          <span className="sk" style={{ width: '28%' }} />
        </div>
      </div>
      <div className="chat-skel-prompt">
        <span className="sk sk-rule" />
        <span className="sk" style={{ width: '46%' }} />
        <span className="sk sk-rule" />
        <span className="sk" style={{ width: '34%' }} />
      </div>
    </div>
  )
}

/** One click: Claude never asks for permission anywhere in this chat's project. */
function BypassProjectButton({ chat }: { chat: Chat }): React.JSX.Element {
  const on = useStore((s) => !!s.projectBypass[folderKey(chat.cwd)])
  useEffect(() => {
    loadProjectBypass(chat.cwd)
  }, [chat.cwd])
  return (
    <button
      className={`btn btn-sm bypass-btn${on ? ' on' : ''}`}
      title={on ? `Claude đang tự làm mọi việc trong dự án ${chat.cwd} mà không hỏi lại — bấm để bắt Claude hỏi trước` : `Cho Claude tự làm mọi việc trong dự án ${chat.cwd}, không cần hỏi lại`}
      onClick={() => toggleProjectBypass(chat.id)}
    >
      {on ? <ShieldOff size={13} /> : <Shield size={13} />}
      <span className="bypass-label">{on ? 'Claude tự làm' : 'Claude hỏi trước'}</span>
    </button>
  )
}

/* ---------------- Model / effort / permission pickers ---------------- */

interface Opt {
  value: string
  label: string
  hint?: string
}

const MODELS: Opt[] = [
  { value: '', label: 'Mặc định', hint: 'Claude Code tự chọn' },
  { value: 'claude-opus-5-5', label: 'Opus 5.5', hint: 'Mạnh nhất' },
  { value: 'claude-sonnet-5-5', label: 'Sonnet 5.5', hint: 'Nhanh, mạnh' },
  { value: 'claude-haiku-5-5', label: 'Haiku 5.5', hint: 'Nhanh nhất' },
  { value: 'claude-fable-5-2', label: 'Fable 5.2' }
]

const EFFORTS: Opt[] = [
  { value: '', label: 'Nghĩ tự động' },
  { value: 'low', label: 'Nghĩ nhanh' },
  { value: 'medium', label: 'Nghĩ vừa' },
  { value: 'high', label: 'Nghĩ kỹ' },
  { value: 'xhigh', label: 'Nghĩ rất kỹ' },
  { value: 'max', label: 'Nghĩ tối đa' }
]

const MODES: Opt[] = [
  { value: '', label: 'Hỏi trước khi làm', hint: 'An toàn nhất' },
  { value: 'acceptEdits', label: 'Tự sửa tệp', hint: 'Chỉ hỏi việc khác' },
  { value: 'plan', label: 'Chỉ lên kế hoạch', hint: 'Không sửa gì' },
  { value: 'bypassPermissions', label: 'Tự làm hết', hint: 'Không hỏi lại' }
]

/** One-click prompts for the chat's project. */
const ACTIONS = [
  {
    key: 'continue',
    label: 'Tiếp tục',
    icon: <FastForward size={12} />,
    title: 'Bảo Claude làm tiếp từ chỗ đã dừng',
    prompt: 'continue',
    confirm: ''
  },
  {
    key: 'resume',
    label: 'Làm lại việc dở',
    icon: <History size={12} />,
    title: 'Bảo Claude tiếp tục việc bị ngắt giữa chừng',
    prompt: 'resume',
    confirm: ''
  },

]

function sendPrompt(id: string, text: string): void {
  writeToTerm(id, text)
  // a separate Enter, so Claude doesn't read it as part of a pasted block
  setTimeout(() => writeToTerm(id, '\r'), 150)
  focusTerm(id)
}

function ChatStatusBar({ chat }: { chat: Chat }): React.JSX.Element {
  const exited = chat.status === 'exited'
  const agent = agentDef(chat.agent)
  const isClaude = agent.id === 'claude'
  // other agents: only "ask" vs their own never-ask flag (Codex --dangerously-bypass…, Gemini --yolo…)
  const modes = isClaude ? MODES : agent.yolo.length ? MODES.filter((m) => !m.value || m.value === 'bypassPermissions') : []
  return (
    <div className="chat-statusbar">
      <div className="sb-actions">
        {ACTIONS.map((a) => (
          <button
            key={a.key}
            className="chip sb-action"
            title={exited ? 'Chat này đã kết thúc — hãy khởi động lại trước' : `${a.label} — ${a.title}`}
            aria-label={a.label}
            disabled={exited}
            onClick={() => {
              if (a.confirm && !window.confirm(a.confirm)) return
              sendPrompt(chat.id, a.prompt)
            }}
          >
            {a.icon}
          </button>
        ))}
      </div>
      {isClaude && <Picker title="Mô hình AI" options={MODELS} value={chat.model ?? ''} onPick={(v) => setChatModel(chat.id, v)} />}
      {isClaude && <Picker title="Claude nghĩ kỹ tới đâu (nghĩ kỹ thì chậm hơn)" options={EFFORTS} value={chat.effort ?? ''} onPick={(v) => setChatEffort(chat.id, v)} />}
      {modes.length > 0 && (
        <Picker
          title="Claude có hỏi bạn trước khi làm không (đổi sẽ khởi động lại chat)"
          options={modes}
          value={chat.permissionMode ?? ''}
          warn={chat.permissionMode === 'bypassPermissions'}
          onPick={(v) => setChatPermission(chat.id, v)}
        />
      )}
    </div>
  )
}

/**
 * Models with a 200k context window: Haiku 4.5 and everything before the 4.6 generation
 * (claude-haiku-4-5-20251001, claude-opus-4-20250514, claude-3-…). Every newer model — Fable,
 * Opus 4.6+, Sonnet 4.6+, Haiku 5.5 and ones not released yet — has 1M.
 */
const SMALL_WINDOW = /-(opus|sonnet|haiku)-4(-[0-5])?(-\d{8})?$|claude-[23]/i

/** How full this chat's context window is, from the last reply in its session file. */
function ContextBar({ chat }: { chat: Chat }): React.JSX.Element {
  const sessionId = chat.sessionId
  const [ctx, setCtx] = useState<{ used: number; model: string } | null>(null)
  useEffect(() => {
    setCtx(null)
    if (!sessionId) return
    let alive = true
    let timer: ReturnType<typeof setTimeout> | undefined
    const load = (): void => {
      window.api.sessions
        .context(sessionId)
        .then((c) => alive && setCtx((prev) => (prev && c && prev.used === c.used && prev.model === c.model ? prev : c)))
        .catch(() => {})
    }
    load()
    const off = window.api.sessions.onChanged((ids) => {
      if (!ids.includes(sessionId) && !ids.includes('*')) return
      clearTimeout(timer)
      timer = setTimeout(load, 400)
    })
    return () => {
      alive = false
      clearTimeout(timer)
      off()
    }
  }, [sessionId])

  const used = ctx?.used ?? 0
  // the model that wrote the last reply decides (it follows a /model typed in the chat); the
  // `[1m]` suffix only exists on the launch flag. A 200k model already past 200k is on 1M too.
  const small = SMALL_WINDOW.test(ctx?.model || chat.model || '') && !/\[1m\]/i.test(chat.model ?? '')
  const max = small && used <= 200_000 ? 200_000 : 1_000_000
  const pct = Math.min(100, (used / max) * 100)
  const level = pct >= 85 ? 'high' : pct >= 60 ? 'mid' : 'low'
  const k = (n: number): string => (n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : `${Math.round(n / 1000)}k`)
  return (
    <div className="ctx-bar" title={ctx ? `Chat này đã dùng ${Math.round(pct)}% bộ nhớ. Gần đầy thì nên mở chat mới.` : 'Chưa có trả lời'}>
      <span className="ctx-label">Bộ nhớ chat</span>
      <div className="ctx-track">
        <div className={`ctx-fill ${level}`} style={{ width: `${pct}%` }} />
      </div>
      <span className="ctx-num">
        {ctx ? `${k(used)} / ${k(max)} · ${Math.round(pct)}%` : '—'}
      </span>
    </div>
  )
}

function Picker(props: { title: string; options: Opt[]; value: string; warn?: boolean; onPick: (v: string) => void }): React.JSX.Element {
  const { title, options, value, warn, onPick } = props
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const close = (e: MouseEvent): void => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false)
    }
    const esc = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setOpen(false)
    }
    window.addEventListener('mousedown', close)
    window.addEventListener('keydown', esc)
    return () => {
      window.removeEventListener('mousedown', close)
      window.removeEventListener('keydown', esc)
    }
  }, [open])
  const cur = options.find((o) => o.value === value) ?? options[0]
  return (
    <div className="sb-picker" ref={ref}>
      <button className={`sb-btn${open ? ' open' : ''}${warn ? ' warn' : ''}`} title={`${cur.label} — ${title}`} onClick={() => setOpen(!open)}>
        {cur.label}
      </button>
      {open && (
        <div className="sb-menu" role="menu">
          <div className="sb-menu-title">{title}</div>
          {options.map((o) => (
            <button
              key={o.value}
              role="menuitemradio"
              aria-checked={o.value === value}
              className={`sb-item${o.value === value ? ' on' : ''}`}
              onClick={() => {
                setOpen(false)
                if (o.value !== value) onPick(o.value)
              }}
            >
              <Check size={13} className="sb-check" />
              <span className="sb-item-label">{o.label}</span>
              {o.hint && <span className="sb-item-hint">{o.hint}</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

function NoChat(): React.JSX.Element {
  const agents = useStore((s) => s.agents)
  const claudeFound = useStore((s) => !!s.claude?.found)
  useStore((s) => s.preferredAgent)
  const def = agentDef(defaultAgent())
  const others = AGENTS.filter((a) => a.id !== def.id && (a.id === 'claude' ? claudeFound : agents.some((x) => x.id === a.id && x.found)))
  return (
    <div className="no-chat">
      <div className="glow-orb">
        <Logo size={56} />
      </div>
      <h3>Hôm nay mình làm gì?</h3>
      <p>Mỗi chat là một phiên {def.label} riêng trong thư mục này. Có thể chạy nhiều chat song song và chuyển qua lại.</p>
      <div className="no-chat-actions">
        <button className="btn btn-primary btn-lg" onClick={() => newChat()}>
          <Sparkles size={16} /> Chat mới{def.id !== 'claude' ? ` · ${def.label}` : ''}
        </button>
        {claudeFound && (
          <>
            <button className="btn btn-ghost" onClick={() => newChat({ args: ['--continue'], title: 'Chat tiếp tục' })}>
              <RotateCcw size={14} /> Tiếp tục chat gần nhất
            </button>
            <button className="btn btn-ghost" onClick={() => newChat({ args: ['--resume'], title: 'Mở lại…' })}>
              <History size={14} /> Mở lại chat cũ…
            </button>
          </>
        )}
      </div>
      {others.length > 0 && (
        <div className="no-chat-actions no-chat-agents">
          {others.map((a) => (
            <button key={a.id} className="btn btn-ghost btn-sm" onClick={() => newChatWith(a.id)}>
              <Plus size={13} /> {a.label}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

function InstallCard(): React.JSX.Element {
  const cmd = 'npm install -g @anthropic-ai/claude-code'
  return (
    <div className="no-chat">
      <div className="glow-orb dim">
        <Logo size={56} />
      </div>
      <h3>Máy chưa cài Claude Code</h3>
      <p>VIBE CODE chạy công cụ <code>claude</code>. Mở PowerShell, chạy lệnh dưới đây để cài, rồi gõ <code>claude</code> để đăng nhập một lần.</p>
      <div className="code-line">
        <code>{cmd}</code>
        <button className="icon-btn" title="Sao chép" onClick={() => navigator.clipboard.writeText(cmd)}>
          <Copy size={14} />
        </button>
      </div>
      <div className="no-chat-actions">
        <button className="btn btn-ghost" onClick={() => detectClaude()}>
          <RefreshCw size={14} /> Mình đã cài xong
        </button>
        <button className="btn btn-ghost" onClick={() => window.api.app.openExternal('https://docs.claude.com/en/docs/claude-code/setup')}>
          <ExternalLink size={14} /> Hướng dẫn
        </button>
      </div>
    </div>
  )
}
