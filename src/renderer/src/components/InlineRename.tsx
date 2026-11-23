import { useLayoutEffect, useRef } from 'react'
import { create } from 'zustand'

/** The one row being renamed in place, by a key its owner picks ('chat:…', 'file:…'). */
export const useRenaming = create<{ key: string | null }>(() => ({ key: null }))

export const startRename = (key: string): void => useRenaming.setState({ key })
const stopRename = (): void => useRenaming.setState({ key: null })

/**
 * Text box that replaces a name while it's renamed: Enter or clicking away saves, Esc cancels.
 * `stem` selects only the part before the extension (file names).
 */
export function RenameInput({ value, onCommit, stem, className }: { value: string; onCommit: (next: string) => unknown; stem?: boolean; className?: string }): React.JSX.Element {
  const ref = useRef<HTMLInputElement>(null)
  const done = useRef(false)

  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    el.focus()
    const dot = stem ? value.lastIndexOf('.') : -1
    el.setSelectionRange(0, dot > 0 ? dot : value.length)
  }, [value, stem])

  const finish = (save: boolean): void => {
    if (done.current) return
    done.current = true
    const next = ref.current?.value.trim() ?? ''
    stopRename()
    if (save && next && next !== value) void onCommit(next)
  }

  return (
    <input
      ref={ref}
      className={`rename-input${className ? ' ' + className : ''}`}
      defaultValue={value}
      spellCheck={false}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        // app shortcuts stay out of the box
        e.stopPropagation()
        if (e.key === 'Enter') finish(true)
        else if (e.key === 'Escape') finish(false)
      }}
      onBlur={() => finish(true)}
    />
  )
}
