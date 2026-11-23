#!/bin/bash
set -euo pipefail

die() {
  printf '\n%s\n' "$*" >&2
  exit 1
}

[[ "$(uname -s)" == Darwin ]] || die "Mở file này trên máy Mac."

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
cd -- "$project_dir"

export PATH="$PATH:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin"

case "$(uname -m)" in
  arm64)
    target=aarch64-apple-darwin
    node_arch=arm64
    ;;
  x86_64)
    if [[ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" == 1 ]]; then
      die "Terminal đang chạy bằng Rosetta. Mở Terminal ở chế độ thường rồi chạy lại."
    fi
    target=x86_64-apple-darwin
    node_arch=x64
    ;;
  *) die "Máy Mac cần chip Apple Silicon hoặc Intel 64-bit." ;;
esac

xcrun --sdk macosx --show-sdk-path >/dev/null 2>&1 ||
  die "Thiếu bộ công cụ Apple. Chạy xcode-select --install trong Terminal, cài xong rồi thử lại."

command -v node >/dev/null && command -v npm >/dev/null ||
  die "Cài Node.js LTS từ https://nodejs.org/en/download rồi mở lại Terminal."
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 24 || (major === 24 && minor >= 11) ? 0 : 1)' ||
  die "Cần Node.js LTS phiên bản 24.11 trở lên."
[[ "$(node -p process.arch)" == "$node_arch" ]] ||
  die "Cài bản Node.js đúng chip của máy Mac rồi thử lại."

command -v rustup >/dev/null && command -v cargo >/dev/null ||
  die "Cài Rust theo https://rustup.rs/ rồi mở lại Terminal."
export RUSTUP_TOOLCHAIN=stable
if ! rustup run stable rustc --version >/dev/null 2>&1; then
  rustup toolchain install stable --profile minimal
fi
rustup target add --toolchain stable "$target"

export CARGO_TARGET_DIR="$project_dir/src-tauri/target"
printf '\nĐang tải thư viện JavaScript…\n'
npm ci --include=dev --include=optional

printf '\nĐang tạo VIBE CODE cho %s. Lần đầu có thể mất vài phút.\n' "$target"
npm run build -- --target "$target" -- --locked

bundle_dir="$CARGO_TARGET_DIR/$target/release/bundle"
app="$bundle_dir/macos/VIBE CODE.app"
dmgs=("$bundle_dir"/dmg/*.dmg)
[[ -d "$app" && -f "${dmgs[0]}" ]] ||
  die "Không tìm thấy bộ cài sau khi tạo app. Kiểm tra $bundle_dir"
codesign --verify --deep --strict "$app"

printf '\nĐã tạo ứng dụng tại %s\n' "$app"
printf 'Bộ cài Mac tại %s\n' "${dmgs[@]}"
open "$bundle_dir/dmg"
