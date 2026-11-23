import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { Plus } from 'lucide-react'
import { AGENTS, type AgentId } from '../../../shared/agents'
import { useStore } from '../store'
import { defaultAgent } from '../actions'

/**
 * "+" that starts a chat right away with the default agent (picked in the status bar).
 * Right-click opens the agent list: Claude Code, Codex, Grok, Gemini… — agents that aren't
 * installed show their install command (click copies it). With no agent installed, click opens the list too.
 */
export function NewChatMenu(props: { title: string; className?: string; size?: number; disabled?: boolean; onPick: (agent: AgentId) => void }): React.JSX.Element {
  const { title, className = 'icon-btn accent', size = 16, disabled, onPick } = props
  const agents = useStore((s) => s.agents)
  const claudeFound = useStore((s) => !!s.claude?.found)
  const [at, setAt] = useState<{ x: number; y: number } | null>(null)
  const btn = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState<React.CSSProperties>({ visibility: 'hidden' })
  const found = (id: AgentId): boolean => (id === 'claude' ? claudeFound : !!agents.find((a) => a.id === id)?.found)
  const def = at ? defaultAgent() : undefined

  const openMenu = (): void => {
    const r = btn.current!.getBoundingClientRect()
    setPos({ visibility: 'hidden' })
    setAt({ x: r.right, y: r.bottom + 4 })
  }

  useLayoutEffect(() => {
    if (!at) return
    const r = menu.current?.getBoundingClientRect()
    if (!r) return
    setPos({
      left: Math.max(4, Math.min(at.x - r.width, window.innerWidth - r.width - 4)),
      top: at.y + r.height > window.innerHeight - 4 ? Math.max(4, at.y - r.height - 30) : at.y
    })
  }, [at])

  useEffect(() => {
    if (!at) return
    const close = (e: MouseEvent): void => {
      if (!menu.current?.contains(e.target as Node) && !btn.current?.contains(e.target as Node)) setAt(null)
    }
    const esc = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') setAt(null)
    }
    const blur = (): void => setAt(null)
    window.addEventListener('mousedown', close, true)
    window.addEventListener('keydown', esc, true)
    window.addEventListener('blur', blur)
    return () => {
      window.removeEventListener('mousedown', close, true)
      window.removeEventListener('keydown', esc, true)
      window.removeEventListener('blur', blur)
    }
  }, [at])

  return (
    <>
      <button
        ref={btn}
        className={className}
        title={title}
        disabled={disabled}
        onClick={(e) => {
          e.stopPropagation()
          if (at) return setAt(null)
          const agent = defaultAgent()
          if (agent) return onPick(agent)
          openMenu()
        }}
        onContextMenu={(e) => {
          e.preventDefault()
          e.stopPropagation()
          openMenu()
        }}
      >
        <Plus size={size} />
      </button>
      {at &&
        createPortal(
          <div ref={menu} className="menu ctx-menu agent-menu" style={pos} onClick={(e) => e.stopPropagation()}>
            <div className="menu-sep">Chat mới với</div>
            {[...AGENTS.filter((a) => found(a.id)), ...AGENTS.filter((a) => !found(a.id))].map((a) => {
              const ok = found(a.id)
              return (
                <button
                  key={a.id}
                  className={`menu-item${ok ? '' : ' missing'}`}
                  title={ok ? `Start a ${a.label} chat in this folder` : `${a.label} isn't installed — click to copy:\n${a.install}`}
                  onClick={() => {
                    setAt(null)
                    if (ok) return onPick(a.id)
                    navigator.clipboard.writeText(a.install).catch(() => {})
                    window.alert(`${a.label} isn't installed.\n\nInstall it from a terminal (copied to the clipboard):\n${a.install}\n\nSign in once by running "${a.cmd}", then reopen VIBE CODE.`)
                  }}
                >
                  <span className="menu-item-main">{a.label}</span>
                  <span className="menu-item-sub">{!ok ? 'not installed' : a.id === def ? 'default' : a.cmd}</span>
                </button>
              )
            })}
          </div>,
          document.body
        )}
    </>
  )
}
