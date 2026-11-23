# VIBE CODE cho sự kiện

## Thử trong app

1. Bấm **Mở Sự kiện** ở màn hình đầu hoặc chọn tab **Sự kiện** trong dự án, rồi bấm **Tạo sự kiện**.
2. Nhập tên, ngày và địa điểm. Bấm **Lưu sự kiện**.
3. Dùng tên và email giả ở mục **Đăng ký thử**. Mỗi email chỉ được đăng ký thử một lần cho mỗi sự kiện.
4. Để thử bằng điện thoại, bấm **Bật link thử cùng Wi-Fi**. Mở link trên điện thoại cùng mạng khi VIBE CODE vẫn đang mở. Nếu Windows hỏi quyền tường lửa, bản thử chỉ cần mạng riêng. Xong thì bấm **Tắt link thử**.
5. Kiểm tra danh sách khách trong app rồi bấm **Xuất CSV**. File CSV mở được trong Excel.
6. Nếu muốn Claude làm một trang riêng, bấm **Nhờ Claude dựng trang riêng**, chọn thư mục dự án và xem lại nội dung trong ô chat. Bấm Enter khi muốn gửi yêu cầu.

Link cùng Wi-Fi dùng cho đăng ký thử bằng dữ liệu giả. Trang đăng ký công khai, check-in và chuyển phiên bản website đang hoạt động cần triển khai ở đợt tiếp theo.

## Đánh dấu đã xong và xóa sự kiện

- **Đánh dấu đã xong** chuyển sự kiện sang danh sách **Đã xong**, giữ khách đã đăng ký và đóng link thử. Dùng **Mở lại** nếu cần nhận đăng ký tiếp.
- **Xóa sự kiện** mở hộp xác nhận và xóa cả danh sách khách của sự kiện. Xuất CSV trước nếu muốn giữ danh sách.
- Trong chat, nhập ở ô soạn và bấm **Gửi** hoặc Enter. Shift+Enter xuống dòng. Lời nhắn và câu trả lời hiện thành hội thoại. Nếu Claude cần bạn xác nhận, bấm **Xem yêu cầu**. **Tùy chọn chat** có chế độ gốc, tệp, tác vụ và thiết lập AI.
- Bấm **Sửa** để đổi tên, ngày hoặc địa điểm mà vẫn giữ danh sách khách. **Nhân bản** điền sẵn tên và địa điểm cho sự kiện mới. Danh sách khách của sự kiện mới bắt đầu trống.
- Có thể tìm sự kiện theo tên hoặc địa điểm, và tìm khách theo tên hoặc email. Ô tìm chấp nhận chữ tiếng Việt có dấu hoặc không dấu. Nút **Xuất CSV** xuất toàn bộ khách của sự kiện.
- Khi đang xem tin cũ, bấm **Mới nhất** để quay về cuối hội thoại.

## Dữ liệu và Zalo

Sự kiện và lượt đăng ký thử được lưu trong `events.json` của hồ sơ VIBE CODE. Đóng rồi mở lại app đọc danh sách từ file đó. Nếu bạn dùng hồ sơ thử riêng, mỗi hồ sơ có file riêng.

Claude vẫn có thể đọc Zalo và phân tích việc. Công cụ gửi Zalo đã được gỡ khỏi cả danh sách MCP và bộ xử lý công cụ. Thông tin đăng nhập, tùy chọn phân tích và kho dữ liệu Zalo giữ cách hoạt động hiện tại.

## Biên dịch trên Windows

Cài một lần Node.js 24 trở lên, Rust MSVC và Visual Studio Build Tools có mục Desktop development with C++. Mở PowerShell trong thư mục này:

```powershell
npm.cmd ci
npm.cmd run build -- --bundles nsis
```

File cài đặt khi build thành công nằm trong `src-tauri/target/release/bundle/nsis`.

## Bảng việc Zalo trên iPhone

Zalo chỉ đăng nhập được ở một nơi, nên máy Mac giữ phiên Zalo và phát bảng **Việc của tôi** qua Wi-Fi cho iPhone.

1. Mở tab **Zalo**, ở khung **Việc của tôi** bấm biểu tượng điện thoại, rồi bấm **Bật bảng cho iPhone**.
2. iPhone dùng cùng Wi-Fi với Mac. Mở Camera và quét mã QR.
3. Trong Safari, bấm **Chia sẻ → Thêm vào MH chính** để mở như một app. Link giữ nguyên sau khi mở lại VIBE CODE, và bảng tự bật lại nếu lần trước đang bật.
4. Trên iPhone: chạm vòng tròn để chuyển Cần làm → Đang làm → Xong, hoặc bấm ↻ để Claude đọc lại tin nhắn. Bảng tự cập nhật mỗi 5 giây.

Lưu ý:
- Giữ VIBE CODE mở và Mac không ngủ. Lần đầu macOS có thể hỏi cho phép kết nối đến; chọn **Cho phép**.
- Ai có link và ở cùng Wi-Fi đều xem được việc. Nếu lỡ gửi link cho người khác, bấm **Đổi link**; link cũ thôi hoạt động.
- Chỉ dùng được trong cùng mạng Wi-Fi. Trả lời tin nhắn vẫn làm trên app Zalo của điện thoại như trước.
