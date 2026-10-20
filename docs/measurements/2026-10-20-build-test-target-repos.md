# Thời gian build + test của repo đích (#2)

Ngày đo: 2026-10-20. Repo đích tạm cho PoC: Rust [typst](https://github.com/hoanghainh1188/typst), Python [scrapy](https://github.com/hoanghainh1188/scrapy) (fork của typst/typst và scrapy/scrapy). Agent chỉ làm việc trên fork, không mở PR lên repo gốc.

**Máy đo:** Apple M4 Pro, 12 nhân, 24 GB RAM, macOS. Đo ở máy local thay vì CI của fork vì sandbox của aegis cũng chạy trên máy (Thiết kế §6–§7). Thời gian là đồng hồ thực, một lần đo, giây làm tròn.

## typst (Rust)

Commit `8162895b3`, rustc 1.99.0. Lệnh như CI của typst: `cargo test --workspace --no-run` rồi `cargo test --workspace --no-fail-fast`.

| Bước | Thời gian | Ghi chú |
| --- | --- | --- |
| `cargo fetch --locked` | 24s | Bước lấy phụ thuộc, có mạng |
| Build test, `target/` trống | 111s | `--offline` sau khi fetch |
| Test toàn bộ workspace | 24s | Pass hết |
| **Build lạnh + test** | **135s (2m15s)** | |
| Sửa 1 file trong `typst-library`, build lại | 7s | `target/` đã cache |
| Test toàn bộ sau khi sửa | 21s | |
| **Vòng sửa (cache ấm)** | **28s** | |
| `cargo test -p typst-library` sau khi sửa | 50s | Chậm hơn `--workspace`: `-p` resolve feature khác nên build lại |

- `target/` sau khi build test: **11 GB**.
- Toàn bộ test chạy offline sau `cargo fetch`.

## scrapy (Python)

Commit `699b96dca`, Python 3.12.2, uv. Không có lockfile: phụ thuộc khai báo trong `pyproject.toml`, phụ thuộc test lấy từ `tox.ini` (`[test-requirements]`).

| Bước | Thời gian | Ghi chú |
| --- | --- | --- |
| `uv venv` + `uv pip install -e .` + phụ thuộc test | 2s | Cache của uv đã ấm; lần đầu sẽ lâu hơn |
| Test toàn bộ, tuần tự (`pytest scrapy tests --doctest-modules`) | 298s (4m58s) | 5914 pass, 538 skip, 88 xfail, **1 fail** |
| Test toàn bộ, song song (pytest-xdist, `auto` worker) | **43s** | Cùng kết quả |
| Một file test (`tests/test_http_request.py`) | <1s | 66 pass |

- Test fail: `tests/test_downloader_handler_aioftp.py::test_cannot_resolve_host` chờ DNS báo lỗi cho `nonexistent.invalid` nhưng truy vấn bị timeout. Lỗi do môi trường mạng, không do code; trong sandbox không mạng kết quả có thể khác nữa.
- 538 test skip vì thiếu phụ thuộc tuỳ chọn (botocore, mitmproxy…), giống env `py3xx` của tox.

## Quyết định

| | typst | scrapy |
| --- | --- | --- |
| Build + test, cache lạnh | 2m15s | 43s (song song) |
| Loại cache cho BP1–BP3 (Thiết kế §4.2: < 3 phút → 5 phút) | **5 phút** | **5 phút** |
| Chế độ > 10 phút (chỉ test phần bị sửa) | Không cần | Không cần |

Hệ quả cho các issue sau:

- **Profile Rust (#11):** với typst, `cargo test -p <crate>` chậm hơn `--workspace` khi `target/` đã cache. Vòng sửa dùng `--workspace`; không dùng chế độ "chỉ test crate bị sửa" cho repo này.
- **Sandbox (#5, #11):** mỗi job cần chỗ cho `target/` riêng khoảng 11 GB với typst. Khởi tạo `target/` từ cache của repo (ADR 0003 §6) là bắt buộc, không thì mỗi job mất ~2 phút build lạnh.
- **RepoConfig của scrapy (#24, #16):**
  - `gate.test` ghi đè thành pytest chạy song song bằng pytest-xdist (`auto` worker) trên `scrapy tests --doctest-modules`. Chạy tuần tự gần 5 phút, vượt thời hạn cache 5 phút.
  - `fetch` ghi đè thành `uv pip install -e .` cộng phụ thuộc test, vì repo không có `uv.lock` nên mặc định `uv sync --frozen` của profile không chạy được.
  - Không có lockfile nên bước chặn phụ thuộc chỉ thấy `pyproject.toml`; bản vá đổi phụ thuộc sẽ bị gate chặn (fail closed).
- **So kết quả test với nhánh chính (#11):** `test_cannot_resolve_host` fail trên nhánh chính do môi trường. Gate phải so với kết quả của nhánh chính trong cùng sandbox, không đòi mọi test pass.
