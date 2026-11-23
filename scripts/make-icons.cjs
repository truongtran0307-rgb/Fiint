// Renders the TINJS Studio logo to PNG without any image dependencies.
// Usage: node scripts/make-icons.cjs  → resources/icon.png (256), resources/tray.png (32)
const fs = require('fs')
const path = require('path')
const zlib = require('zlib')

function crc32(buf) {
  let c, crc = 0xffffffff
  for (let n = 0; n < buf.length; n++) {
    c = (crc ^ buf[n]) & 0xff
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    crc = (crc >>> 8) ^ c
  }
  return (crc ^ 0xffffffff) >>> 0
}
function chunk(type, data) {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const td = Buffer.concat([Buffer.from(type), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(td))
  return Buffer.concat([len, td, crc])
}
function png(size, rgba) {
  const raw = Buffer.alloc(size * (size * 4 + 1))
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4)
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(size, 0)
  ihdr.writeUInt32BE(size, 4)
  ihdr[8] = 8
  ihdr[9] = 6
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0))
  ])
}

// Shapes in a 32×32 design space (same as the in-app SVG logo).
const inRoundRect = (x, y, rx, ry, w, h, r) => {
  if (x < rx || y < ry || x > rx + w || y > ry + h) return false
  const cx = Math.min(Math.max(x, rx + r), rx + w - r)
  const cy = Math.min(Math.max(y, ry + r), ry + h - r)
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r
}
function sample(x, y) {
  if (!inRoundRect(x, y, 1, 1, 30, 30, 8)) return null
  const white =
    inRoundRect(x, y, 7.5, 8, 15, 3.8, 1.9) || inRoundRect(x, y, 13.1, 8, 3.8, 16, 1.9)
  if (white) return [255, 255, 255]
  if (inRoundRect(x, y, 19.5, 20.6, 5.5, 3.4, 1.4)) return [250, 214, 200]
  const t = (x + y) / 64 // diagonal gradient #f0a07c → #c8573a
  const base = [240 + (200 - 240) * t, 160 + (87 - 160) * t, 124 + (58 - 124) * t]
  if (inRoundRect(x, y, 1.5, 1.5, 29, 14, 7.5)) return base.map((v) => v + (255 - v) * 0.1)
  return base
}
function render(size) {
  const ss = 4
  const out = Buffer.alloc(size * size * 4)
  for (let py = 0; py < size; py++)
    for (let px = 0; px < size; px++) {
      let r = 0, g = 0, b = 0, a = 0
      for (let sy = 0; sy < ss; sy++)
        for (let sx = 0; sx < ss; sx++) {
          const c = sample(((px + (sx + 0.5) / ss) / size) * 32, ((py + (sy + 0.5) / ss) / size) * 32)
          if (c) {
            r += c[0]
            g += c[1]
            b += c[2]
            a++
          }
        }
      const i = (py * size + px) * 4
      if (a) {
        out[i] = r / a
        out[i + 1] = g / a
        out[i + 2] = b / a
        out[i + 3] = (a / (ss * ss)) * 255
      }
    }
  return png(size, out)
}

const dir = path.join(__dirname, '..', 'resources')
fs.mkdirSync(dir, { recursive: true })
fs.writeFileSync(path.join(dir, 'icon.png'), render(256))
fs.writeFileSync(path.join(dir, 'tray.png'), render(32))
fs.writeFileSync(path.join(dir, 'icon-mac.png'), render(1024)) // macOS .icns needs >= 512px
console.log('icons written to', dir)
