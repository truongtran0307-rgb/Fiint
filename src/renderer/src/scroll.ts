/**
 * Scroll `el` into view inside `box` only.
 * Element.scrollIntoView also scrolls every ancestor — including overflow:hidden panels —
 * which shoves whole tabs/panes out of place. This touches nothing but `box`.
 */
export function revealIn(
  box: HTMLElement | null | undefined,
  el: Element | null | undefined,
  opts: { block?: 'nearest' | 'center'; inline?: 'nearest'; smooth?: boolean } = {}
): void {
  if (!box || !el) return
  const b = box.getBoundingClientRect()
  const r = el.getBoundingClientRect()
  let top = box.scrollTop
  let left = box.scrollLeft
  if (opts.block === 'center') top += r.top - b.top - (b.height - r.height) / 2
  else if (opts.block === 'nearest') {
    if (r.top < b.top) top += r.top - b.top
    else if (r.bottom > b.bottom) top += r.bottom - b.bottom
  }
  if (opts.inline === 'nearest') {
    if (r.left < b.left) left += r.left - b.left
    else if (r.right > b.right) left += r.right - b.right
  }
  if (top === box.scrollTop && left === box.scrollLeft) return
  box.scrollTo({ top, left, behavior: opts.smooth ? 'smooth' : 'auto' })
}
