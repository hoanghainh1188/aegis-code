# Test của repo đích trong sandbox không mạng (#5)

Ngày đo: 2026-10-21. Máy như [#2](2026-10-20-build-test-target-repos.md): Apple M4 Pro. Docker Desktop 29.9.1 với VM aarch64, 12 CPU, 8 GB RAM. Image: `docker/rust` (rust 1.99, rustfmt, clippy, cargo-llvm-cov 0.9.1, cargo-deny 0.20.2), `docker/python` (python 3.12, ruff 0.17.1, pytest 9.1.1, pytest-xdist, pytest-cov, coverage, uv 0.13.4).

Lệnh: `scripts/sandbox-test.sh <profile> <repo>`.
- Phụ thuộc được lấy trước, trong một container có mạng, vào cache trên host (`.workspace/state/cache/<repo>/`). Bước này #66 sẽ thay bằng bản đi qua proxy registry.
- Sau đó bước test của profile chạy với `--network none --cap-drop ALL --security-opt no-new-privileges`, dưới user của host, chỉ mount cây repo ở `/work` và các thư mục cache.

| | typst (Rust) | scrapy (Python) |
| --- | --- | --- |
| Lấy phụ thuộc (có mạng, cache ấm) | 1s (lần đầu 21s) | 1s |
| Test, không mạng | 359s (build lạnh) | 91s (`-n auto`) |
| Kết quả | 218 pass, 0 fail, 15 ignored (39 binary test) | 6483 pass, 23 fail, 102 error, 474 skip |

## Những gì phải sửa để chạy được

- **Phụ thuộc lấy từ git** (typst: `codex`, `hayro`, `typst-assets`): Cargo lưu chúng ở `$CARGO_HOME/git`. Profile Rust nay cache cả `/usr/local/cargo/git` bên cạnh `registry`.
- **Gói build C/C++:** `openssl-sys` cần `pkg-config` và `libssl-dev`, một crate khác cần `g++`. Đã thêm vào image Rust.
- **Bộ nhớ:** VM 8 GB không link nổi các binary test lớn của typst khi chạy 12 job với debuginfo đầy đủ (linker bị SIGKILL).
  - Bỏ `--tmpfs /tmp`, vì file tạm của linker khi đó nằm trong RAM. Container `--rm` vẫn có `/tmp` riêng cho mỗi lần chạy.
  - Với typst: `CARGO_BUILD_JOBS=2` và `CARGO_PROFILE_{DEV,TEST}_DEBUG=line-tables-only`. Lifecycle đặt được các biến này qua `[run] sandbox_env`.
- **`target/` của host:** clone typst có sẵn bản build macOS từ #2, nên script cho container dùng `target/sandbox`. Job thật chạy trên worktree mới nên không gặp chuyện này.

## Test fail khi không có mạng (scrapy)

Đều là test cần mạng:
- 112 test của `tests/test_downloader_handler_twisted_http11.py` và `test_downloader_handler_aiohttp.py`: proxy, mitmproxy, IPv6.
- 14 doctest trong `docs/` gọi `books.toscrape.com` và gặp lỗi DNS.

Chúng fail như nhau trên base, nên gate (so với base, #11) không coi là lỗi mới. RepoConfig của scrapy có thể bỏ hẳn các file này khỏi `gate.test` để chạy nhanh hơn.

## Hệ quả

- **#66:** bước lấy phụ thuộc chạy trong container riêng, có mạng nhưng chỉ ra được registry, và ghi vào đúng các thư mục cache này. Với Python là `.venv` trong cây job, cùng cache uv.
- **`[run] sandbox` trong `config/poc.toml`** để `false` cho tới khi có #66, vì worktree của job chưa có phụ thuộc. Khi bật với typst, đặt `sandbox_env` như trên.
- **Thời gian:** build lạnh typst trong VM mất 6 phút, so với 2 phút trên host (#2). Cache `target/` theo repo (ADR 0003 §6) càng quan trọng hơn khi chạy trong container.
