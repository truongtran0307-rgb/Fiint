import { useState } from 'react'
import { initials, peerGradient } from './util'

/** Round avatar: the photo, or Telegram-style initials on the peer's gradient. */
export function Avatar({ id, name, src, size = 40, group }: { id: string; name: string; src?: string; size?: number; group?: boolean }): React.JSX.Element {
  const [broken, setBroken] = useState<string | null>(null)
  const ok = src && broken !== src
  return (
    <span className={'zl-avatar' + (group ? ' group' : '')} style={{ width: size, height: size, fontSize: Math.round(size * 0.38), background: ok ? undefined : peerGradient(id) }}>
      {ok ? <img src={src} alt="" draggable={false} loading="lazy" referrerPolicy="no-referrer" onError={() => setBroken(src)} /> : initials(name)}
    </span>
  )
}

export function TypingDots(): React.JSX.Element {
  return (
    <span className="zl-dots" aria-hidden>
      <i />
      <i />
      <i />
    </span>
  )
}
