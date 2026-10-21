# Bộ thử của PoC (#4)

Issue đã đóng của các repo đích, dùng để đo KPI ở Proposal §8 (#18). Mỗi issue một file `issues/<repo>-<số>.toml`, gồm:
- commit base để chạy lại (`base_commit`);
- bản sửa của người (`fix_commit`, `fix_pr`);
- các test mà bản sửa thêm vào (`hidden_tests`);
- nội dung issue lúc lấy về (`body`).

Harness chạy bộ thử là #98.

## Cách chọn

Bộ được dựng lại bằng:
- `scripts/eval/collect.py <owner/repo> <ngôn ngữ> <từ ngày>`: lấy ứng viên;
- `scripts/eval/build.py`: viết các file `.toml`;
- `scripts/eval/verify.py`: kiểm từng issue.

1. Issue đóng với lý do "completed" trong 6 tháng (từ 2026-04-21), bởi một PR đã merge trong chính repo đó.
2. PR vừa sửa mã nguồn (`.rs`/`.py`, ngoài `docs/` và `tests/`) vừa thêm hoặc đổi test.
3. PR không đụng phụ thuộc, toolchain hay CI: lockfile, manifest, `.github/`, `tox.ini`, `rust-toolchain`. Gate chặn các thay đổi đó (#12, #83), nên những issue này không đo được agent.
4. PR đổi tối đa 12 file và 300 dòng mã nguồn.
5. Issue có nhãn `bug` hoặc `regression` và đóng từ 2026-06-01, để gần với việc bảo trì (§5.2) và để model ít khả năng đã thấy bản sửa.
6. Mỗi PR chỉ một issue.
7. Kiểm bằng `verify.py`: ở `base_commit`, chép riêng các file test của bản sửa vào thì test ẩn **fail**; ở `fix_commit` thì **pass**. Issue qua bước này có bảng `[verified]`.

Bước 1–6 cho 21 issue scrapy và 20 issue typst. Sau bước 7 còn **32 issue: 18 typst, 14 scrapy**. Các issue bị bỏ:

| Issue | Lý do |
| --- | --- |
| typst-6183 | Test là Rust trong `crates/typst-cli/tests`, không chạy qua harness của typst |
| typst-8679 | So base với bản sửa thì không thấy file test |
| scrapy-210, scrapy-7796 | Test ẩn đã pass ngay ở base. 7796 cần Python 3.14 (PEP 649) nên bị skip trên 3.12 |
| scrapy-3552 | Test ở base treo (chờ `--pdb`), quá 30 phút |
| scrapy-5794 | Ở cả bản sửa, module test vẫn không import được |
| scrapy-7260, scrapy-7626 | Ở cả bản sửa vẫn fail: chúng chạy `scrapy` như tiến trình con và kiểm log, vốn phụ thuộc môi trường |
| scrapy-7601 | Không có test nào chạy: handler httpx cần phụ thuộc mà môi trường đo không có |

Môi trường kiểm scrapy: Python 3.12, `test-requirements` và `extra-deps` của `tox.ini`, trừ `google-cloud-storage` và các shell tương tác.

## Định dạng

| Trường | Nghĩa |
| --- | --- |
| `id` | `<repo>-<số issue>`, cũng là id trong ledger và nhánh `agent/<id>` |
| `language` | Profile (`rust`, `python`), để báo KPI theo từng ngôn ngữ (ADR 0004) |
| `repo`, `issue`, `url`, `title`, `closed_at`, `labels` | Issue gốc |
| `base_commit` | Cha thứ nhất của commit merge bản sửa: code ngay trước khi sửa |
| `fix_commit`, `fix_pr` | Bản sửa của người |
| `source_files`, `test_files` | File mà bản sửa đổi |
| `hidden_tests` | typst: tên test trong `tests/suite`; scrapy: file test cho pytest. Agent không bao giờ thấy |
| `body` | Nội dung issue lúc lấy về, để lần chạy nào cũng đọc đúng một văn bản |
| `[verified]` | Kết quả của `verify.py` |

Lệnh chạy test ẩn:
- typst: `cargo test --workspace --test tests -- "^(tên1|tên2)$"`
- scrapy: `python -m pytest -q -p no:cacheprovider <file>…`

## Lưu ý

typst và scrapy là repo OSS, tạm dùng cho PoC; Proposal §2 chốt dùng repo nội bộ. Model có thể đã thấy code của chúng lúc huấn luyện, nên báo cáo #18 phải ghi rõ điều này. Bước 5 chỉ giảm bớt rủi ro, không loại bỏ được.
