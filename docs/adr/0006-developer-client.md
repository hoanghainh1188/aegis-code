# ADR 0006 – Client cho developer: `aegis-job`

- Ngày: 2026-10-28
- Trạng thái: Đã chấp nhận
- Người quyết định: @Nguyen Han Hoang Hai
- Issue: #231

## Bối cảnh

Developer làm việc với aegis-code qua tracker (Jira/Backlog), webhook (nhãn `aegis`, CI hỏng, review trên PR/MR) và dashboard. Không có client nào chạy trên máy developer. `aegis` (crate `aegis-cli`) là công cụ vận hành và eval: nó chạy cả pipeline tại chỗ, nên cần Docker, khóa model, token GitHub và file config, và chỉ build được từ source. `/api/v1` của `aegis-server` đã có đủ thao tác trên job (tạo, xem, lượt chạy, huỷ, chạy lại, vòng review), nhưng chỉ nhận token của service (`aegis-server token <service>`), không có danh tính người dùng.

## Quyết định

1. **Đăng nhập bằng token cá nhân.**
   - Người đăng nhập dashboard qua SSO tạo token riêng ở `/me/tokens`: có tên, hết hạn sau tối đa 90 ngày, tối đa 10 token còn hạn mỗi người.
   - Token chỉ hiện một lần, trên trang vừa tạo, không bao giờ nằm trong URL; server chỉ giữ hash, như token service.
   - Request mang token này làm việc với tư cách người đó: đúng vai trò của họ trên từng repo, audit ghi tên họ. Người đó thu hồi được từng token; token hết hạn bị từ chối ngay.
   - Không làm OIDC device flow lúc này: nó phụ thuộc SSO của công ty có hỗ trợ device authorization grant hay không, và cần thêm bước đổi token SSO lấy token API. Có thể thêm sau trong một issue riêng mà không đổi API.

2. **Binary riêng `aegis-job`, crate `aegis-client`.**
   - Chỉ phụ thuộc HTTP (reqwest, rustls) và JSON; không kéo agent, gate, Docker hay provider model, nên nhỏ, build nhanh và cài không cần gì khác.
   - Chỉ gọi `/api/v1`, thêm `GET /api/v1/me` và `GET /api/v1/repos` (các repo người gọi có vai trò, kèm vai trò) để client biết mình là ai và đổi slug thành id.
   - Lệnh: `login`, `logout`, `whoami`, `repos`, `submit`, `list`, `show`, `watch`, `cancel`, `retry`, `review`, `open`; `--json` cho script.
   - Token chỉ gửi qua `https://`, hoặc `http://` tới chính máy (trial). Login lưu ở `~/.config/aegis/job.toml`, quyền 600; `AEGIS_JOB_SERVER` và `AEGIS_JOB_TOKEN` thay file cho CI.

3. **Phân phối: GitHub Releases và `cargo install`.**
   - Tag `aegis-job-v<phiên bản>` chạy workflow build binary cho macOS (arm64, x86_64) và Linux (x86_64), đóng `tar.gz` kèm SHA-256, gắn vào GitHub Release.
   - Người có Rust cài bằng `cargo install --git … aegis-client`.
   - Homebrew tap và gói npm bọc binary để sau, khi có người dùng ngoài team.
   - **Cập nhật (#263):** client nay phát hành cùng version với server (`vX.Y.Z`, [docs/release.md](../release.md)), thay cho tag `aegis-job-v*`. Có thêm gói npm `@hoanghainh1188/aegis-job`: một gói chứa sẵn binary cho bốn nền tảng (thêm Linux arm64) và một launcher JS, không tải gì khi cài.

## Hệ quả

- Developer cài một binary, tạo token trên dashboard, rồi giao và theo dõi job từ terminal mà không cần quyền vận hành. Audit phân biệt được người giao với service.
- Token cá nhân nằm trên máy developer, nên có hạn và thu hồi được. Lộ token thì người đó thu hồi trên `/me/tokens`; admin vẫn thu hồi được mọi phiên bằng `aegis-server revoke-sessions`, còn token cá nhân cần thu hồi từng cái (chưa có lệnh thu hồi hàng loạt cho người).
- Dashboard cần `public_url` để trang token chỉ đúng địa chỉ cho `aegis-job login`.
- Kiểm tra cuối của #231 (giao job lên staging) chờ #33; đã thử trên trial (`scripts/trial/`).
