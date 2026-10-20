# Độ chính xác của Codebase-Memory trên repo đích (#3)

Ngày đo: 2026-10-20. Công cụ: [codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp) v0.12.1 (MIT), bản build `darwin-arm64` đã kiểm SHA-256 với `checksums.txt`. Chạy qua CLI (`codebase-memory-mcp cli query_graph …`) với `HOME` riêng trong `.workspace/cbm-home`, không chạy `install` (lệnh này sửa cấu hình MCP client). Repo đích như [#2](2026-10-20-build-test-target-repos.md): typst `8162895b3`, scrapy `699b96dca`.

## Lập chỉ mục

| | typst (Rust) | scrapy (Python) |
| --- | --- | --- |
| Lập chỉ mục đầy đủ | 3s | 3s |
| Lập lại sau khi sửa 1 file | — | 2,4s |
| Cạnh nhiều nhất | USAGE 31159, CALLS 19497 | CALLS 10912, USAGE 8040 |
| Cạnh TESTS | 580 | 4260 |

SQLite của cả hai repo: 94 MB. Chỉ mục gồm cả `docs/` (`.rst`, `.html`), nên có cạnh từ tài liệu tới class.

## Đối chiếu với grep

Ground truth là grep trên mã nguồn rồi đọc từng chỗ khớp. Đơn vị đếm là hàm gọi hoặc file.

### Rust (typst)

| Quan hệ | Ca kiểm | Đồ thị | Thực tế | Ghi chú |
| --- | --- | --- | --- | --- |
| CALLS | Ai gọi `typst_syntax::parse` | 9 hàm: 5 đúng, 4 sai | 8 hàm | 4 sai là `str::parse` ở `ast.rs`, `package.rs` bị khớp theo tên. Sót cả 3 chỗ gọi từ crate khác (`typst-library/text/raw.rs`, `typst-eval/lib.rs`, `typst-eval/call.rs`) |
| CALLS | Ai gọi `calc::abs` (`#[func]`) | 6 hàm, sai cả 6 | 0 (chỉ gọi từ mã Typst) | Đều là `f32::abs`, `.abs()` của số |
| CALLS | Ai gọi `World::source` (trait) | 5 | ~33 chỗ `.source(` | Gọi qua trait object/generic hầu như không nối được |
| IMPLEMENTS | `impl Reflect for` | 22 | 29 viết tay + ~213 sinh bởi `cast!`/`#[ty]` | Không thấy impl do macro sinh ra |
| USAGE | File dùng `SyntaxNode` | 10 | 12 (trừ file định nghĩa) | |
| TESTS | Mẫu 8 cạnh | Nhiễu | | Đích là tham số như `arg`, `lines` (không có file) |

### Python (scrapy)

| Quan hệ | Ca kiểm | Đồ thị | Thực tế | Ghi chú |
| --- | --- | --- | --- | --- |
| CALLS | File gọi `load_object` | 31 | 31 | Khớp hoàn toàn |
| IMPORTS | File import `scrapy.utils.misc` | 105 | 105 | Khớp hoàn toàn |
| INHERITS | Lớp con trực tiếp của `Request` | 5 | 5 | Khớp hoàn toàn |
| TESTS | Test cho `load_object` | 10 test | 3 test trực tiếp | 3 test trực tiếp có đủ; 7 còn lại là test gọi `load_object` gián tiếp, vẫn hữu ích |
| DECORATES | `@pytest.fixture` | Có | | Decorator được ghi thành cạnh |
| Import động | `"scrapy.downloadermiddlewares.robotstxt.RobotsTxtMiddleware"` trong `default_settings.py` | Không có cạnh | Có | Chuỗi đường dẫn (`load_object`, settings, `mock.patch("…")`) không được phân giải |

Thêm một hàm vào `scrapy/http/request/form.py` gọi `load_object` **mà không import**: đồ thị vẫn nối cạnh tới `scrapy/utils/misc.py`. Tức là CALLS được phân giải theo tên duy nhất trong repo, không theo import. Với Python đúng phần lớn vì tên ít trùng; với Rust, tên như `parse`, `abs`, `new` trùng nhiều nên sai.

### Lập chỉ mục lại

`index_repository` chạy lại sau khi sửa file: nhận hàm mới, và sau khi hoàn tác thì nút cũ bị xoá (0 kết quả). Lập lại cả repo chỉ 2–3 giây nên không cần dựa vào cơ chế cập nhật theo hash: ACI lập lại sau mỗi lần sửa là đủ rẻ.

## Quyết định

| | Rust | Python |
| --- | --- | --- |
| Dùng đồ thị cho | Tìm ứng viên nhanh: định nghĩa, file liên quan, test gần | Tìm nơi gọi, import, lớp con, test liên quan |
| Cần LSP | **Có**, `rust-analyzer` cho find-references/go-to-definition trước khi sửa một hàm hoặc trait | Không bắt buộc cho Phase 1; `basedpyright` chỉ khi trùng tên |
| Bổ sung grep | Tên hàm sinh bởi macro (`#[func]`, `cast!`) | Chuỗi đường dẫn dotted (`"pkg.mod.Class"`) cho import động, settings, `mock.patch` |

Hệ quả cho các issue sau:

- **ACI (#7):** công cụ "ai gọi hàm này" trả kết quả đồ thị kèm độ tin cậy theo ngôn ngữ. Với Rust, kết quả phải qua `rust-analyzer` (`aci.lsp` trong profile) trước khi agent dựa vào đó để sửa chữ ký hàm. Với Python, cộng thêm grep chuỗi đường dẫn dotted của symbol.
- **Không dùng TESTS của Rust để chọn test (#11):** cạnh nhiễu. Gate vẫn chạy `--workspace` (#2 đã cho thấy nhanh hơn `-p`).
- **Lập chỉ mục lại sau mỗi bản vá:** 2–3 giây, không cần cơ chế cập nhật riêng.
- **Loại `docs/` khỏi chỉ mục nếu gây nhiễu:** dùng `.cbmignore`; để quyết khi làm #7.
