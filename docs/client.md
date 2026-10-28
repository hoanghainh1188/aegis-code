# `aegis-job`: giao và theo dõi job từ terminal

Client cho developer ([ADR 0006](adr/0006-developer-client.md)). Nó chỉ gọi API của `aegis-server`; agent, gate và model đều chạy trên server. Bạn chỉ cần một token của chính mình.

## Cài

**npm** (macOS arm64/x86_64, Linux arm64/x86_64; cần Node 18+):

```bash
npm install -g @hoanghainh1188/aegis-job
```

Gói chứa sẵn binary cho bốn nền tảng, không tải gì lúc cài. Cùng phiên bản với server (`vX.Y.Z`, [docs/release.md](release.md)).

**Binary:** tải `aegis-job-<version>-<target>.tar.gz` ở trang Releases của repo (tag `v…`), kiểm SHA-256 rồi đặt vào `PATH`:

```bash
shasum -a 256 -c aegis-job-<version>-<target>.tar.gz.sha256
tar -xzf aegis-job-<version>-<target>.tar.gz
install aegis-job-<version>-<target>/aegis-job ~/.local/bin/
```

**Từ source** (cần Rust stable):

```bash
cargo install --locked --git https://github.com/hoanghainh1188/aegis-code aegis-client
```

## Đăng nhập

1. Mở dashboard (`https://<server>/`), đăng nhập SSO, vào **Token** (`/me/tokens`). Đặt tên (ví dụ `laptop`), chọn số ngày (tối đa 90), bấm **Tạo token**. Token chỉ hiện một lần: chép ngay.
2. Trên máy:

   ```bash
   aegis-job login https://<server>
   ```

   Dán token khi được hỏi (không hiện ra màn hình). Login được lưu ở `~/.config/aegis/job.toml`, chỉ bạn đọc được.
3. Kiểm tra: `aegis-job whoami` và `aegis-job repos` (các repo bạn có vai trò, kèm vai trò).

Token làm việc với đúng quyền của bạn: giao, huỷ, chạy lại job cần vai trò maintainer trên repo đó. Mất máy hoặc lộ token: thu hồi trên `/me/tokens`. Cho CI hay script: đặt `AEGIS_JOB_SERVER` và `AEGIS_JOB_TOKEN` thay vì `login`.

## Giao và theo dõi

```bash
aegis-job submit <repo> <issue>        # vd. aegis-job submit typst typst/typst#123
aegis-job watch <job>                  # in mỗi lần trạng thái đổi; thoát 0 khi có PR
aegis-job show <job>                   # job, các lượt chạy, lý do kết thúc
aegis-job open <job>                   # mở PR, hoặc trang job trên dashboard
aegis-job list --repo <repo> --status running
```

- `<issue>`: `owner/repo#12` là issue GitHub, `KEY-12` là ticket Jira. Còn lại thì nói rõ nguồn: `--source gitlab` (`group/project#12`), `--source backlog`.
- `aegis-job cancel <job>`, `aegis-job retry <job>` (job thất bại hoặc đã huỷ), `aegis-job review <job>` (vòng sửa theo review của PR mà job đã mở, #243).
- Thêm `--json` vào bất kỳ lệnh nào để có đúng dữ liệu server trả về, cho script.

## Mã thoát

| Mã | Nghĩa |
| --- | --- |
| 0 | Thành công; với `watch`: job kết thúc và đã mở PR |
| 1 | `watch`: job kết thúc mà không có PR (dừng, thất bại, bị huỷ) |
| 2 | Lỗi: sai cú pháp, chưa đăng nhập, server từ chối (kèm lý do) |

## Thử trên máy

Trên trial ([local-trial.md](local-trial.md)): `up.sh` xong thì đăng nhập dashboard tại <http://localhost:8080>, tạo token ở **Token**, rồi `aegis-job login http://localhost:8080` và `aegis-job submit demo demo-org/demo#1`.
