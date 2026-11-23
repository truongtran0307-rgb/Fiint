# VIBE CODE — bản build riêng

Giữ nguyên mã nguồn VIBE CODE v0.2.8 (bản gốc), chỉ sửa những chỗ sau:

- **Tắt tự cập nhật** (`src-tauri/src/appcmds.rs`): app không còn gọi `api.github.com/repos/nguyentrantin98/vibecode`, không tải hay chạy installer nào. Nút "Update" sẽ không bao giờ hiện.
- **Xoá `scripts/release.cjs`** và lệnh `npm run release`: script này commit, push và đăng bản phát hành lên repo GitHub của tác giả.
- **Xoá `.github/`**: workflow CI của repo gốc.
- **Gọn lại cho thiết kế / sự kiện** (không dành cho lập trình):
  - Bỏ trình soạn code Monaco, Git (Changes, diff, commit, nhánh), tab Database (SQL Server) và panel Terminal.
  - Tab "Source code" đổi thành **Tệp**: duyệt thư mục dự án, bấm vào tệp thì mở bằng ứng dụng mặc định của Windows.
  - Bỏ các nút "Commit + push", "Pull", "Run App", "Publish BE" trong khung chat.
  - App không còn tự cài plugin lập trình (context7, feature-dev, các LSP) và skill karpathy-guidelines vào Claude Code.
  - Thêm 8 mẫu việc sự kiện (Bắt đầu nhanh + nút "Mẫu việc sự kiện"), Claude được dặn trả lời tiếng Việt theo vai trợ lý sự kiện.
  - Giao diện chính chuyển sang tiếng Việt. Phím tắt tab: Ctrl+1 Tệp, Ctrl+2 Web, Ctrl+3 Zalo.
  - `components/Browser.tsx` (khung trang web của tab Web) được viết lại vì bản gốc bị xoá nhầm khi thử bỏ tab Web.

Các kết nối mạng còn lại đều đi thẳng tới dịch vụ bạn đang dùng, không qua tác giả:
Zalo (tab Zalo), `api.anthropic.com` (xem hạn mức Claude), marketplace plugin chính thức của Claude Code,
Google (gợi ý tìm kiếm, favicon bookmark) và picsum.photos (ảnh nền tab trống của trình duyệt).

Mã app (`vn.tinjs.vibecode`) và thư mục dữ liệu (`%APPDATA%\TINJS Studio`) giữ nguyên, nên khi cài đè
lên bản cũ, mọi cài đặt, lịch sử chat, phiên đăng nhập Zalo và kết nối database vẫn còn.

## Cài công cụ (chỉ làm một lần)

1. **Node.js 24+**: https://nodejs.org
2. **Visual Studio Build Tools**: https://visualstudio.microsoft.com/visual-cpp-build-tools/
   → chọn mục **"Desktop development with C++"**.
3. **Rust**: https://rustup.rs → chạy `rustup-init.exe`, chọn mặc định (toolchain MSVC).

Mở lại PowerShell rồi kiểm tra: `node -v`, `cargo -V`.

## Build

```powershell
cd <thư mục đã giải nén>
npm install
npm run build
```

Lần đầu build mất khoảng 5–15 phút (Rust phải biên dịch các thư viện). Khi xong, file cài đặt nằm ở:

```
src-tauri\target\release\bundle\nsis\VIBE CODE_0.2.8_x64-setup.exe
```

Tắt VIBE CODE đang chạy (cả icon dưới khay hệ thống), rồi chạy file đó để cài đè lên bản cũ.

## Kiểm tra (tuỳ chọn)

Sau khi cài, mở PowerShell và chạy lệnh dưới. Nếu không ra dòng nào thì bản build không còn chứa địa chỉ repo của tác giả:

```powershell
Select-String -Path "$env:LOCALAPPDATA\VIBE CODE\vibe-code.exe" -Pattern "nguyentrantin98" -SimpleMatch
```

Lưu ý: SmartScreen có thể cảnh báo vì file chưa ký số. Đây là bản bạn tự build nên bấm "More info → Run anyway".
