import { useEffect, useState } from 'react'
import { File, FileArchive, FileCode, FileSpreadsheet, FileText, Folder, Paperclip, X } from 'lucide-react'
import { clearAttachments, formatSize, removeAttachment, useAttachments, type Attachment } from '../attachments'
import { focusTerm } from '../terminals'

const EMPTY: Attachment[] = []

function iconFor(a: Attachment): React.JSX.Element {
  if (a.dir) return <Folder size={16} />
  const ext = (a.name.split('.').pop() ?? '').toLowerCase()
  if (/^(zip|rar|7z|tar|gz|tgz|bz2|xz)$/.test(ext)) return <FileArchive size={16} />
  if (/^(xlsx?|csv|tsv|ods)$/.test(ext)) return <FileSpreadsheet size={16} />
  if (/^(md|txt|log|pdf|docx?|rtf)$/.test(ext)) return <FileText size={16} />
  if (/^(ts|tsx|js|jsx|mjs|cjs|json|py|cs|java|go|rs|c|cc|cpp|h|hpp|rb|php|html|css|scss|sql|sh|ps1|xml|ya?ml|toml|vue|svelte|kt|swift)$/.test(ext))
    return <FileCode size={16} />
  return <File size={16} />
}

/** Files waiting to go out with the next message: thumbnails / file cards, each removable. */
export function AttachmentTray({ chatId }: { chatId: string }): React.JSX.Element | null {
  const items = useAttachments((s) => s.byChat[chatId] ?? EMPTY)
  const [zoom, setZoom] = useState<string | null>(null)

  useEffect(() => {
    if (!zoom) return
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== 'Escape') return
      e.preventDefault()
      e.stopPropagation()
      setZoom(null)
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [zoom])

  if (!items.length) return null
  const remove = (key: string): void => {
    removeAttachment(chatId, key)
    focusTerm(chatId)
  }

  return (
    <div className="attach-tray">
      <div className="attach-head">
        <Paperclip size={12} />
        <span>
          {items.length} {items.length === 1 ? 'file' : 'files'} · sent with your next message (Enter)
        </span>
        <button
          className="attach-clear"
          onClick={() => {
            clearAttachments(chatId)
            focusTerm(chatId)
          }}
        >
          Clear all
        </button>
      </div>
      <div className="attach-list">
        {items.map((a) =>
          a.image && a.preview ? (
            <div key={a.key} className="attach-item attach-thumb" title={`${a.path}\n${formatSize(a.size)}`}>
              <button className="attach-thumb-img" onClick={() => setZoom(a.preview!)}>
                <img src={a.preview} alt={a.name} draggable={false} />
              </button>
              <button className="attach-x" title="Bỏ" onClick={() => remove(a.key)}>
                <X size={11} />
              </button>
            </div>
          ) : (
            <div key={a.key} className="attach-item attach-file" title={a.path}>
              <span className="attach-file-icon">{iconFor(a)}</span>
              <span className="attach-file-text">
                <span className="attach-file-name">{a.name}</span>
                <span className="attach-file-size">{a.dir ? 'Folder' : formatSize(a.size) || '—'}</span>
              </span>
              <button className="attach-x" title="Bỏ" onClick={() => remove(a.key)}>
                <X size={11} />
              </button>
            </div>
          )
        )}
      </div>
      {zoom && (
        <div className="image-lightbox" onClick={() => setZoom(null)}>
          <img src={zoom} alt="" onClick={(e) => e.stopPropagation()} />
          <button className="lb-btn lb-close" title="Đóng (Esc)" onClick={() => setZoom(null)}>
            <X size={18} />
          </button>
        </div>
      )}
    </div>
  )
}
