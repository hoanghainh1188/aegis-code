# Chạy khô bộ thử: phát lại bản sửa thật qua toàn bộ pipeline (#104)

Ngày đo: 2026-10-21 → 2026-10-22. Máy như [#2](2026-10-20-build-test-target-repos.md): Apple M4 Pro, Docker Desktop 29.9 với VM aarch64, 12 CPU, 8 GB RAM. Image dựng lại từ `docker/rust` và `docker/python` như [#5](2026-10-21-sandbox-test-target-repos.md).

Lệnh:

```bash
aegis eval --config <config với sandbox = true> --set eval/issues --replay --no-pr
```

- **Không gọi model.** `ReplayProvider` đóng vai Light và Core. Core ghi `source_files` của `fix_commit` qua công cụ ACI `edit` / `create_file`, rồi trả câu trả lời cuối dạng JSON. Nếu bản sửa còn đổi test có sẵn (bỏ `xfail`, đổi giá trị mong đợi) và mọi file đổi đều là text, Core ghi luôn các test đó và khai báo trong `allowed_test_changes`, như agent thật phải làm.
- **Mọi bước còn lại chạy thật:** fetch qua egress proxy (#66), gate trong sandbox `--network none` (#5), hidden test (#98).
- **Ledger riêng** (`state_dir/ledger-replay`), run id `<id>-dry<n>`, nên không lẫn với KPI và job dir của lượt chạy model. `--no-pr` dừng ở chỗ sẽ mở PR.

## Kết quả

| | scrapy (Python) | typst (Rust) |
| --- | --- | --- |
| Issue | 14 | 18 |
| Qua gate và hidden test | **14/14** | **14/14** trong số các issue không cần #105 |
| Bị chặn vì phải cập nhật tham chiếu test (#105) | 0 | 4 (7777, 8372, 8826, 8850) |
| Thời gian một issue | 5–9 phút | 14–22 phút với cache build ấm, ~39 phút lạnh |

scrapy, lượt cuối cùng đạt của mỗi issue:

| Issue | Thời gian | Ghi chú |
| --- | --- | --- |
| scrapy-1280 | 516s | Bản sửa đổi test có sẵn trong `tests/test_crawl.py` |
| scrapy-2702 | 463s | |
| scrapy-3095 | 431s | |
| scrapy-3666 | 397s | |
| scrapy-3778 | 291s | |
| scrapy-4330 | ~550s | 24 hidden test `TestMitmProxy` cần chương trình `mitmdump`, ghi vào `hidden_known_failures` |
| scrapy-4466 | 366s | Bản sửa bỏ `xfail(strict)` trong `tests/test_crawl.py` |
| scrapy-4729 | 334s | |
| scrapy-4999 | 325s | |
| scrapy-7029 | 557s | |
| scrapy-7597 | 498s | Hidden test cần Pillow, chỉ có trong env `extra-deps` của tox |
| scrapy-7604 | 389s | |
| scrapy-7730 | 400s | |
| scrapy-8186 | 381s | |

typst, lượt cuối cùng đạt của mỗi issue (cache build ấm):

| Issue | Thời gian | Ghi chú |
| --- | --- | --- |
| typst-2639 | 1147s | |
| typst-3582 | 888s | |
| typst-6822 | 952s | |
| typst-7219 | 872s | |
| typst-7541 | 1042s | Bản sửa thêm ảnh tham chiếu mới: lượt chạy khô không ghi test, hidden test chép vào sau gate |
| typst-7777 | — | Bị chặn bởi #105 (3 test có sẵn đổi tham chiếu) |
| typst-8372 | — | Bị chặn bởi #105 (25 test) |
| typst-8476 | 885s | |
| typst-8560 | 885s | |
| typst-8616 | 892s | Test mới và tham chiếu mới đều là text: lượt chạy khô ghi cả hai |
| typst-8648 | 868s | |
| typst-8672 | 867s | |
| typst-8703 | 1282s | |
| typst-8725 | 1351s | |
| typst-8826 | — | Bị chặn bởi #105 (3 test) |
| typst-8828 | 1172s | |
| typst-8850 | — | Bị chặn bởi #105 (368 test) |
| typst-8857 | 1015s | |

## Những gì lượt chạy khô tìm ra và đã sửa

Các lỗi dưới đây đều sẽ làm hỏng lượt chạy model thật, tốn tiền mà KPI không nói lên điều gì.

**Hạ tầng (mọi repo):**
- **Formatter của `edit` chạy trên máy host.** Máy không có `ruff` hay `rustfmt` thì mọi lần sửa đều bị từ chối. Nay khi bật sandbox, formatter chạy trong image của profile, không mạng, có mount `.venv` để cùng phiên bản với gate.
- **Format, lint, typecheck fail ngay ở base.** Ví dụ ruff mới hơn bản repo đang pin báo 115 lỗi có sẵn. Nay áp dụng đúng "không cảnh báo mới" của thiết kế §6.1: base cũng fail thì chỉ tính các phát hiện mới trong file bản vá đổi, đã bỏ số dòng để dòng dịch chỗ vẫn khớp.
- **Bước coverage fail vì test fail.** Test fail đã được bước test so với base, nên coverage chỉ cần ra được báo cáo. Rust dùng thêm `--ignore-run-fail` cho `cargo llvm-cov`.
- **Parser pytest:**
  - Đọc nhầm dòng log có sẵn (`ERROR    scrapy…0xffff…`) thành tên test. Nay chỉ đọc phần "short test summary".
  - Không nhận ra dòng tổng kết khi pytest chạy quá một phút (`… in 73.06s (0:01:13)`).
- **Test flaky** (`tests/test_crawl.py`): khi có fail mới, gate chạy lại test một lần và chỉ tính các test fail cả hai lần.
- **Harness test riêng của typst.** Harness in `❌ <tên> (<file>:<dòng>)` thay cho `test … FAILED`, nên trước đây gate coi cả binary `tests` là "chưa chạy xong" ở cả hai phía, hai bên triệt tiêu nhau: một bản vá làm vỡ test của typst vẫn qua gate. Nay có `gate.test_failure` và `gate.test_finished` (regex), đọc thêm trên parser libtest.
- **Hidden test cần mạng hoặc chương trình ngoài** thì fail cả với bản sửa thật. Lượt chạy khô ghi các test này ra `hidden-failures.txt`, và `hidden_known_failures` trong file của issue khiến chúng không bị tính là lỗi của agent.
- **Lượt chạy khô ghi nửa thay đổi test.** Ví dụ ghi `.typ` có test mới mà thiếu ảnh tham chiếu mới, hoặc thiếu file tham chiếu `.txt` mới. Nay thay đổi test của bản sửa được ghi trọn hoặc không ghi gì. Nếu có file nhị phân thì không ghi gì, và hidden test vẫn chép đủ vào sau gate.
- **Cache build theo repo** (ADR 0003): `target/` của mỗi job và của bản base bắt đầu từ bản clone copy-on-write của cache (`cp -c` trên APFS). Chỉ bản base, tức code upstream, ghi lại cache, nên bản vá không để lại gì cho job sau. Thời gian typst giảm từ khoảng 39 xuống khoảng 17 phút một issue.

**Cấu hình theo repo:**
- **scrapy** (`config/poc.toml`):
  - Phụ thuộc test lấy từ `tox.ini` tại đúng commit: `[test-requirements]`, `[testenv]`, `[testenv:extra-deps]`.
  - Cài ruff đúng bản `.pre-commit-config.yaml` pin vào `.venv`.
  - Bỏ các test `requires_internet`: chúng chờ DNS hết hạn, làm một lượt test chậm từ 90s lên 10 phút.
  - `TLDEXTRACT_PUBLIC_SUFFIX_LIST_URLS=""` để tldextract không tải danh sách khi chạy. Trước đó cảnh báo của nó lọt vào test nào chạy đầu tiên.
- **typst** (`config/typst.toml`):
  - `fetch_hosts` thêm `github.com`: typst lấy 3 phụ thuộc từ git.
  - `edit_format` với edition 2024.
  - Regex cho harness test của typst.
  - `sandbox_env` cho VM 8 GB như #5.
- Cả `config/poc.toml` và `config/typst.toml` nay đặt `sandbox = true`, để lượt chạy model (#6, #18) chạy đúng như lượt chạy khô.

## Còn lại

- **#105:** bản sửa đổi output của test có sẵn thì phải cập nhật file tham chiếu (ảnh `.png`, `hashes.txt`). Công cụ chỉ ghi được text, nên mọi agent đều fail gate ở 4/18 issue typst. Cho tới khi #105 xong, 4 issue này được tính là "nền tảng chưa hỗ trợ" khi đọc KPI typst.
- **Docker Desktop:** VM bị treo một lần khi đang chạy, và một lần đầy đĩa (~25 GB `target/` trong lúc build typst). Lượt chạy dài cần đủ chỗ trống, khoảng 30 GB trở lên.
- **typst-7541 trên base:** `import-nested-item` fail sẵn trong sandbox, như nhau ở cả hai phía, nên gate không tính là lỗi.
