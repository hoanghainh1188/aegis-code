# Thiết kế kỹ thuật – PoC

Oct 17, 2026 · @Nguyen Han Hoang Hai

Tài liệu này mô tả kiến trúc kỹ thuật của phần lõi (Phase 1 – MVP / PoC), phục vụ mục tiêu xây platform đầy đủ theo [ADR 0002](../adr/0002-full-platform-mvp-as-checkpoint.md) và tuân thủ các ràng buộc kiến trúc cho Phase 2 theo [ADR 0003](../adr/0003-platform-service-architecture.md) §7. Dev làm theo tài liệu này. Nếu mâu thuẫn lộ trình, ADR 0002 và ADR 0003 là chuẩn.

## 1. Phạm vi & giả định

| Mục | Giá trị |
| --- | --- |
| Repo | Repo nội bộ, > 100k dòng, ngôn ngữ nào cũng được qua language profile ([ADR 0004](../adr/0004-multi-language-profiles.md)); PoC dùng một repo Rust và một repo Python |
| Test | Lệnh test của repo, chạy trên CI hiện có |
| Thời gian build | Chưa đo — đo ở Phase 0 |
| Ngôn ngữ platform | Rust, Cargo workspace (`crates/`) — xem [ADR 0001](../adr/0001-platform-in-rust.md) |
| Agent harness | Tự làm bằng Rust; gọi model qua trait `ModelProvider` (Claude API qua HTTP, Bedrock qua AWS SDK for Rust) |
| Chỉ mục repo | Đã chọn Codebase-Memory MCP. Code hiện chưa dùng: ACI tìm theo AST và dựng module map bằng tree-sitter của profile (mục 3.2) |
| Sandbox | Docker, mỗi profile một image (dựa trên image CI của repo nếu có), không có mạng ra ngoài; phụ thuộc lấy ở bước riêng qua proxy registry |
| Sonar | Hỗ trợ cả SonarQube Server và SonarQube Cloud. Bước Sonar của gate chạy khi cấu hình có `[sonar]` (#91), không thì ghi "bỏ qua" |
| Đầu ra | Nhánh `agent/<issue-id>` + PR; người merge |

**Hai cấu hình chạy:**

- **PoC (repo nội bộ):** gọi Claude API trực tiếp. Không ràng buộc vùng.
- **Code khách (chỉ ở Nhật):** gọi Bedrock từ `ap-northeast-1` qua profile `jp.`. Chỉ dùng model có profile Nhật.

Mọi chỗ khác nhau giữa hai cấu hình chỉ nằm trong file cấu hình, không nằm trong code.

## 2. Thành phần & vòng lặp xử lý

**Thành phần:**

| Thành phần | Việc làm | Crate / Triển khai |
| --- | --- | --- |
| Orchestrator | Nhận issue, gọi các bước, giữ trạng thái, tính tiền | Tự làm (`crates/aegis-orchestrator`, vòng lặp tool-use riêng) |
| Model provider | Gọi Claude API / Bedrock, đặt `cache_control`, chặn model ID không có `jp.` ở cấu hình khách | Tự làm (`crates/aegis-provider`) |
| Router | Chọn model theo tầng và luật leo thang, kiểm soát ngân sách | Tự làm (`crates/aegis-router`) |
| ACI tools | Tìm, xem, sửa code | Tự làm (`crates/aegis-aci`, MCP server Rust, dùng tree-sitter + language server theo profile) |
| Language profile | Mọi phần phụ thuộc ngôn ngữ của repo đích: grammar, kiểm tra khi `edit`, bước gate, lockfile, sandbox, quy ước prompt | Tự làm (`crates/aegis-lang`, [ADR 0004](../adr/0004-multi-language-profiles.md)) |
| Codebase-Memory MCP | Đồ thị gọi hàm của repo | Dùng sẵn (SQLite index). **Chưa nối:** đã đo ở #3, chưa crate nào gọi (mục 3.2) |
| Gate runner | Chạy format, lint, test, coverage, Sonar trong Docker (Sonar: #91) | Tự làm (`crates/aegis-gate`) |
| PR writer | Mở PR, viết giải thích từng đoạn sửa | Tự làm (`crates/aegis-pr`) |
| Ledger | Ghi log, token, chi phí cho từng issue | Tự làm (`crates/aegis-ledger`, trait ghi JSON / DB) |
| Sonar batcher | Chia nhóm cảnh báo Sonar, gộp nhánh integration | Tự làm (`crates/aegis-sonar-batch`, #17; lifecycle chạy các nhóm trong `aegis-lifecycle`) |

**Vòng lặp cho một issue:**

1. Light đọc issue, phân loại, tóm tắt log đính kèm.
2. Tạo nhánh `agent/<issue-id>` từ nhánh chính.
3. Core tìm vị trí lỗi bằng ACI và module map. Theo thiết kế còn có Codebase-Memory, nhưng chưa nối (mục 3.2).
4. Core viết test tái hiện. Chạy để chắc là FAIL.
5. Core viết bản vá.
6. Gate runner chạy chuỗi kiểm chứng trong Docker (6 bước ở mục 6.1).
7. Qua hết → PR writer mở PR. Dừng.
8. Fail → log được Light tóm tắt, trả lại Core. Quay lại bước 5.
9. Chạm luật leo thang → chuyển sang Expert, tiếp tục từ bước 3 với toàn bộ lịch sử.
10. Chạm luật dừng → ghi lý do, xóa nhánh, giao người.

## 3. Tầng 1 – Lấy ngữ cảnh

### 3.1 Bộ công cụ ACI

Mọi công cụ trả kết quả có định dạng rõ, kèm đường dẫn và số dòng. Không trả output thô của shell.

| Công cụ | Đầu vào | Đầu ra | Giới hạn |
| --- | --- | --- | --- |
| `search_symbol` | Tên hàm / struct / trait / impl | Chữ ký, file, dòng bắt đầu–kết thúc | Tối đa 50 kết quả; quá 50 → báo agent thu hẹp |
| `search_text` | Chuỗi, phạm vi thư mục | File + dòng khớp | Tối đa 50 kết quả |
| `find_file` | Tên hoặc mẫu tên file | Danh sách đường dẫn | Tối đa 50 kết quả |
| `open` | File, dòng bắt đầu | Khung 100 dòng có số dòng, số dòng ẩn trên/dưới | 100 dòng / lần |
| `scroll_up` / `scroll_down` / `goto` | Hướng hoặc số dòng | Khung 100 dòng mới | — |
| `edit` | File, dòng từ–đến, nội dung mới | Đoạn sau khi sửa ±5 dòng | Kiểm tra cú pháp bằng tree-sitter + formatter của profile (ví dụ `rustfmt`, `ruff format`); lỗi → từ chối, không ghi file |
| `run_tests` | Tên một test (như test runner gọi) | PASSED / FAILED, test fail, phần cuối output | Lệnh `gate.repro_test` của profile, trong sandbox của gate (`--network none`, image và mount của profile); dừng sau `step_timeout_s`, tối đa 240 s. Không phải lần gate, không chạy shell tự do (#137) |

Khung 100 dòng và giới hạn 50 kết quả lấy theo thử nghiệm của SWE-agent: 100 dòng cho kết quả tốt hơn 30 dòng, 400 dòng và cả file.

### 3.2 Chỉ mục Codebase-Memory MCP

- Lập chỉ mục một lần ở Phase 0. Lưu vào file SQLite trong workspace của PoC.
- Đồ thị gồm các quan hệ: `CALLS`, `IMPORTS`, `USAGES`, `IMPLEMENTS`, `INHERITS`, `TESTS`.
- Khi code đổi, chỉ lập chỉ mục lại file có hash thay đổi.
- Agent dùng qua MCP: tìm hàm gọi / được gọi, tìm test liên quan, xem cụm module.
- Bổ sung language server của profile (`rust-analyzer`, `basedpyright`…) cho "đi tới định nghĩa" và "tìm chỗ dùng" khi đồ thị chưa đủ chính xác (Rust: macro, trait generic; Python: decorator, import động).
- Việc của Phase 0: kiểm tra Codebase-Memory phân tích đúng các trường hợp trên ở repo Rust và repo Python thật (#3).

**Trạng thái (22/10): đã đo, chưa nối vào code.**

- Kết quả đo ở #3 ([số liệu](../measurements/2026-10-20-codebase-memory-accuracy.md)):
  - **Python (scrapy):** CALLS, IMPORTS, INHERITS khớp hoàn toàn với grep. Đồ thị không thấy import động qua chuỗi đường dẫn (`load_object`, settings, `mock.patch`).
  - **Rust (typst):** CALLS nối theo tên. Vì vậy `parse`, `abs` khớp nhầm sang hàm khác, còn lời gọi qua trait gần như không được nối. Đồ thị cũng không thấy impl do macro sinh ra. Muốn sửa chữ ký hàm hay trait thì phải kiểm lại bằng `rust-analyzer`.
- Code hiện tại không gọi Codebase-Memory, cũng chưa gọi language server:
  - `search_symbol`, `search_text`, `find_file` của ACI tìm bằng truy vấn symbol tree-sitter của profile (`aegis-lang`).
  - Module map ở BP2 (mục 4.1) cũng dựng bằng tree-sitter: thư mục nguồn và các định nghĩa chính trong từng thư mục. Map dựng một lần cho mỗi repo và lưu vào file.
  - Trường `aci.lsp` đã có trong profile nhưng ACI chưa dùng.
- Hệ quả: agent không có công cụ "ai gọi hàm này" hay "test nào liên quan", và luật leo thang "lỗi trải trên ≥ 3 file" (mục 5.3) không có nguồn tín hiệu.
- **Còn mở, chưa có issue:** nối Codebase-Memory qua MCP theo quyết định 3 của [ADR 0001](../adr/0001-platform-in-rust.md), dùng language server thay thế, hay giữ tree-sitter. Nên chốt sau lượt chạy model thật đầu tiên (#6): nếu ledger cho thấy agent định vị lỗi sai nhiều thì mới đáng làm.

### 3.3 Thu gọn lịch sử

- Luôn giữ nguyên: suy nghĩ và hành động của agent ở mọi lượt.
- Chỉ thu gọn **kết quả công cụ** (nội dung file, log) của các lượt cũ.
- Kích hoạt khi context vượt 60% cửa sổ. Khi đó thu gọn một lần mọi kết quả cũ hơn 5 lượt gần nhất thành `[Observation collapsed]`.
- Sau khi thu gọn, chấp nhận ghi cache lại một lần. Không thu gọn sau mỗi lượt.

### 3.4 Chia nhóm issue Sonar

Áp dụng khi đầu vào là danh sách cảnh báo Sonar thay vì một bug.

- **Cảnh báo một vị trí:** tìm node AST chứa vị trí đó (hàm / impl). Gom các cảnh báo cùng node vào một nhóm. Mỗi nhóm là một phiên agent.
- **Cảnh báo nhiều vị trí (luồng dữ liệu nguồn → lan truyền → điểm dùng):** mỗi luồng là một nhóm riêng.
- Các nhóm chạy song song, mỗi nhóm một nhánh. Gộp ở mục 6.3.

**Cách làm (#17):**

- Danh sách cảnh báo là response của `/api/issues/search`: lấy từ khối ` ```json ` trong issue, không có thì đọc tối đa 100 issue đang mở của project trên server ở mục `[sonar]`. Không có cả hai thì issue dừng, ghi lý do.
- Node AST lấy bằng query symbol của profile (`aegis_lang::symbols_in`, cùng query với `search_symbol` của ACI): hàm hoặc method nhỏ nhất chứa dòng cảnh báo. Không có hàm thì lấy impl / class / trait / module nhỏ nhất. Không có cả hai, hoặc file không đọc được, thì gom theo file.
- Cảnh báo có `flows` (vị trí phụ) là cảnh báo luồng dữ liệu, mỗi cái một nhóm.
- Mỗi batch chạy tối đa 8 nhóm. Nhóm vượt hạn được ghi "chưa sửa" trong PR.
- Mỗi nhóm là một phiên Core/Expert như issue bug, từ base, trên worktree riêng, ledger riêng `<issue>-<nhóm>`. Vì vậy trần $4 / 40 lượt tính **cho từng nhóm**. Qua gate thì cây đã kiểm được commit vào clone làm đầu nhánh của nhóm, không mở PR. Ledger của issue nhận lại lượt gọi model và lượt gate của các nhóm. Chỉ ledger của issue kết thúc, nên KPI đếm cả batch là một issue.

## 4. Tầng 2 – Prompt caching

### 4.1 Bố cục prompt và 4 điểm ngắt

| Thứ tự | Nội dung | Thay đổi khi nào | Điểm ngắt |
| --- | --- | --- | --- |
| 1 | Tool definitions (ACI + MCP) | Gần như không bao giờ | BP1 |
| 2 | System prompt + quy ước code theo profile và của dự án + tóm tắt cụm module | Khi đổi repo | BP2 |
| 3 | Nội dung issue + log ban đầu + test tái hiện | Mỗi issue | BP3 |
| 4 | Lịch sử các bước, kết quả công cụ | Mỗi lượt | BP4 = tự động, dời dần về cuối |

- Cache gắn với từng model. Khi leo thang Core → Expert, lượt đầu ở Expert phải ghi cache mới. Tính khoản này vào ngân sách.
- Mỗi lượt kiểm tra `cache_read_input_tokens`. Từ lượt 2 trở đi mà bằng 0 → có lỗi làm vỡ cache, ghi cảnh báo vào log.

### 4.2 Thời hạn cache

- Thời hạn tính từ lúc bắt đầu request, kể cả thời gian stream response.
- Thời gian build + test (đo ở Phase 0) quyết định loại cache cho BP1–BP3:
  - Dưới 3 phút: cache 5 phút.
  - Từ 3 phút trở lên: cache 1 giờ.
- BP4 luôn dùng 5 phút.

### 4.3 Công thức chi phí input

```latex
C_{in} = R \cdot (N_{read} \cdot M_{read} + N_{write} \cdot M_{write} + N_{uncached})
```

R là giá input cơ sở của model. M\_read là 0.05 với Opus/Sonnet 5.5, 0.1 với Haiku 5.5. M\_write là 1.25 (5 phút) hoặc 2 (1 giờ). Ledger tính theo đúng công thức này cho từng request.

### 4.4 Checklist tránh vỡ cache

- [ ] Không chèn ngày giờ, session ID, issue ID vào BP1–BP2.
- [ ] Serialize JSON với key đã sắp xếp.
- [ ] Danh sách tool cố định suốt phiên.
- [ ] Tóm tắt cụm module ở BP2 sinh một lần cho mỗi repo, lưu file, không sinh lại mỗi issue.
- [ ] Bedrock: dùng điểm ngắt thủ công nếu cache tự động không được hỗ trợ; kiểm tra ngưỡng token tối thiểu của Bedrock.

## 5. Tầng 3 – Chọn model

### 5.1 Model ID theo nơi chạy

| Tầng | PoC – Claude API | Code khách – Bedrock (`ap-northeast-1`) |
| --- | --- | --- |
| Light | `claude-haiku-5-5` | `jp.anthropic.claude-haiku-5-5` |
| Core | `claude-sonnet-5-5` | `jp.anthropic.claude-opus-5-5`, effort thấp |
| Expert | `claude-opus-5-5`, effort cao | `jp.anthropic.claude-opus-5-5`, effort cao |

Khi Sonnet 5.5 có profile `jp.`, đổi Core của cấu hình Bedrock sang Sonnet. Chỉ sửa file cấu hình.

### 5.2 Phân loại issue (Light)

Light trả về JSON cố định:

```json
{"kind": "bug | ci_failure | sonar", "has_repro_info": true, "suspected_crates": ["..."], "summary": "..."}
```

- `has_repro_info = false` → vẫn chạy, nhưng Core phải tự viết test tái hiện trước khi sửa.
- `kind = sonar` → đi qua bước chia nhóm (mục 3.4) và gộp nhánh (mục 6.3).
- Light không quyết định leo thang. Mọi issue bắt đầu ở Core.

### 5.3 Luật chuyển trạng thái

| Từ | Điều kiện | Đến |
| --- | --- | --- |
| Core | Gate qua hết | PR |
| Core | `edit` bị từ chối 2 lần liên tiếp | Expert |
| Core | Test tái hiện vẫn fail sau 2 bản vá liên tiếp | Expert |
| Core | Codebase-Memory cho thấy lỗi trải trên ≥ 3 file (router có luật này nhưng chưa có nguồn tín hiệu, mục 3.2) | Expert |
| Core | Đã dùng 15 lượt gọi mà chưa có bản vá nào tới test tái hiện hay gate (#127) | Expert |
| Core, Expert | Đã dùng 8 lượt gọi (rồi 16) mà chưa gọi `run_tests` lần nào (#144) | Nhắc agent viết và chạy test tái hiện, ở lại tầng đó |
| Expert | Gate qua hết | PR |
| Expert | Fail 2 lần | Dừng |
| Bất kỳ | Chi phí cộng dồn > $4 hoặc > 40 lượt gọi model | Dừng |

Khi chuyển sang Expert, giữ toàn bộ lịch sử. Lượt gọi Light không tính vào giới hạn 40.

Giới hạn ban đầu là 20 lượt. Lượt chạy 10 issue đầu của bộ thử (#18, đo ngày 2026-10-23) cho thấy trần $4 không bao giờ chạm (cao nhất $0.54), còn 6/10 issue dừng vì hết 20 lượt. Ca qua gate nộp bản vá đầu sau 7–13 lượt, ca fail sau 17–18 lượt hoặc không nộp. Vì vậy nâng lên 40 lượt và thêm luật 15 lượt chưa có bản vá (#127).

Lời nhắc (#144) dừng vòng lặp trước lượt gọi kế tiếp, thêm một đoạn văn bản vào cuối tin nhắn chứa kết quả công cụ, rồi gọi tiếp ở cùng tầng. Nó không tốn lượt gọi, và mỗi job có tối đa 2 lời nhắc. Khi đo lại ở #142, agent đã có `run_tests` (#137) nhưng chỉ 6/11 lượt có gọi, phần lớn từ lượt 31–40; 4 lượt chỉ đọc code tới hết 40 lượt. Ca duy nhất gọi sớm (lượt 6) cũng là ca nộp gate sớm nhất.

### 5.4 Tính ngân sách

- Ledger cộng chi phí thật từ `usage` sau mỗi request (công thức mục 4.3 + output).
- Trước mỗi lượt gọi Core/Expert: nếu chi phí đã dùng + ước tính lượt tới > $4 → dừng luôn, không gọi.
- Với cấu hình Bedrock, nhân thêm phụ phí vùng (khoảng +10%) khi tính.
- Trần $4 là giá trị khởi đầu cho PoC nội bộ. Ở quy mô mở rộng, ngưỡng kinh tế tối đa cho phép trung bình ≤ $8/issue (bao gồm cả các ca thất bại và phụ phí vùng Bedrock), chốt lại theo số liệu thực tế đo được (ADR 0002).

## 6. Tầng 4 – Kiểm chứng

### 6.1 Chuỗi lệnh của gate runner

Chạy tuần tự trong Docker. Bước nào fail thì dừng, trả log (đã tóm tắt) về agent. Lệnh của từng bước lấy từ language profile của repo ([ADR 0004](../adr/0004-multi-language-profiles.md)); `RepoConfig` ghi đè bằng lệnh CI của chính repo; bước repo không có thì ghi "bỏ qua". Bảng dưới lấy profile Rust làm ví dụ.

| Bước | Lệnh (profile Rust) | Điều kiện qua |
| --- | --- | --- |
| 0. Phụ thuộc | Chỉ khi bản vá đổi manifest / lockfile: kiểm phụ thuộc mới theo allow-list, advisory, license (`cargo deny`); qua thì lấy phụ thuộc trong container riêng có proxy registry | Không phụ thuộc lạ |
| 1. Format | `cargo fmt --check` | Không có diff |
| 2. Lint | `cargo clippy --all-targets -- -D warnings` | Không cảnh báo mới |
| 3. Test tái hiện | `cargo test <tên test tái hiện>` | Chuyển từ FAIL sang PASS |
| 4. Test toàn bộ | `cargo test --workspace` | PASS như trên nhánh chính |
| 5. Coverage | `cargo llvm-cov --workspace --lcov --output-path lcov.info` | Có file coverage |
| 6. Sonar | `sonar-scanner` (nhập báo cáo linter + coverage) | Quality Gate "Sonar way" trên code mới (#91). Không có `[sonar]` trong cấu hình thì "bỏ qua, chưa cấu hình" |

- Profile Python dùng `ruff format --check`, `ruff check`, `pytest`, `coverage.py` (Cobertura). Profile .NET gộp bước Sonar vào bước build (SonarScanner for .NET).
- Nếu build + test > 10 phút: bước 4 chỉ chạy phần bị sửa (`cargo test -p <crate>`, `pytest <thư mục>`) trong vòng lặp; chạy toàn bộ một lần trước khi mở PR.
- Cache thư mục build và cache phụ thuộc của profile giữa các lần chạy (Rust: `target/` + registry Cargo).
- Sonar Server và Sonar Cloud khác nhau ở URL, token, và khóa tổ chức (Cloud). Đặt trong mục `[sonar]` của file cấu hình; token đọc từ biến môi trường `token_env`, chỉ đưa cho lệnh scanner và Web API, không vào môi trường của bước khác (#91).
- Bước Sonar (#91):
  1. Scanner đọc thuộc tính từ file gate tự viết (`[sonar.properties]` của profile, rồi của cấu hình, rồi của gate: URL, project, PR), không đọc `sonar-project.properties` của repo: bản vá không tự loại file của mình khỏi phân tích hay đổi server nhận token được.
  2. Mặc định phân tích bản vá như một pull request vào nhánh gốc (key `aegis-<12 ký tự đầu của tree>`), Quality Gate chấm phần code đổi. Cần SonarQube Cloud hoặc Server từ Developer Edition. Community Build đặt `pull_request = false`: phân tích nhánh chính, chấm theo new-code period của project.
  3. Chạy trong sandbox: scanner chạy trong image riêng (mặc định `sonarsource/sonar-scanner-cli`) có mạng `network` (mặc định `bridge`) để tới server; scanner không chạy code của bản vá. Profile .NET: `dotnet sonarscanner begin` → `[sonar] build` của profile (trong sandbox không mạng, không token) → `end`.
  4. Chờ background task qua `api/ce/task` (hạn `wait_s`, nhận tín hiệu huỷ), kiểm task đúng project và PR, đọc `api/qualitygates/project_status` và issue mới (`api/issues/search`).
  5. Quality Gate fail hoặc scanner fail → bước fail, agent nhận các điều kiện fail và tối đa 20 issue mới. Server không tới được, từ chối token, phân tích lỗi, quá hạn, hoặc project không có Quality Gate → lỗi gate (`GateError::Sonar`), job dừng: agent không sửa được những lỗi này.
- Chưa kiểm với server Sonar thật: test dùng scanner giả và server giả (wiremock). Còn mở: SCM trong container (file `.git` của worktree trỏ ra ngoài `/work`), cách SonarScanner for .NET đọc `SONAR_TOKEN`.
- KPI đo khi cấu hình chưa có `[sonar]`: bản vá chỉ qua 3 trong 4 cổng ở Proposal §6, phần chất lượng code mới chỉ được kiểm bằng lint ("không cảnh báo mới") và người duyệt. Báo cáo KPI phải ghi rõ điều này.

### 6.2 Golden Master cho code chưa có test

1. Trước khi sửa: agent viết test ghi lại đầu ra hiện tại của hàm bị nghi ngờ, với nhiều đầu vào.
2. Chạy trên code gốc. Phải PASS hết.
3. Sau khi sửa: test nào đổi kết quả thì agent phải liệt kê trong PR, mục "Test được phép đổi", kèm lý do.
4. Người duyệt xác nhận từng test trong danh sách đó.

### 6.3 Gộp nhiều bản vá (issue Sonar chia nhóm)

`aegis_sonar_batch::integrate` (#17); batch id là id của issue.

1. Tạo nhánh `agent/integration-<batch-id>`.
2. Gộp từng nhánh nhóm vào, mỗi lần một nhánh.
3. Sau mỗi lần gộp, chạy lại gate runner.
4. Xung đột hoặc gate fail → bỏ nhánh vừa gộp, chạy lại nhóm đó riêng trên nhánh integration mới.
5. Gộp xong hết → mở một PR cho cả batch.

- Chạy lại chỉ một lần. Lần chạy lại vẫn xung đột hoặc gate vẫn fail thì bỏ nhóm. Phiên của nhóm dừng ngay từ đầu (ngân sách, agent dừng) thì cũng bỏ nhóm, không chạy lại. PR ghi nhóm nào đã sửa, nhóm nào đã chạy lại và nhóm nào bị bỏ kèm lý do.
- Gate sau mỗi lần gộp so với base ban đầu, không có test tái hiện, và được phép đổi các test mà các nhóm đã khai. Gate cả batch không qua lần nào thì issue dừng, không mở PR.
- PR là một commit của cây đã qua gate cuối cùng trên base, đẩy lên `agent/integration-<batch-id>` của fork, như PR của issue bug.
- Đã kiểm bằng test: batch mẫu 5 cảnh báo Python có xung đột, gate fail và nhóm dừng (`crates/aegis-sonar-batch/tests/sample_batch.rs`, phiên và gate giả), cùng một issue Sonar 2 nhóm chạy hết vòng đời với gate Rust thật (`crates/aegis-lifecycle/tests/sonar_batch.rs`, model theo kịch bản). Chưa chạy với model thật và server Sonar thật.

### 6.4 Mẫu PR

```markdown
## Issue
<link issue>

## Nguyên nhân
<1–3 câu>

## Thay đổi (theo từng đoạn)
- <file:dòng> — <sửa gì, vì sao>

## Test
- Test tái hiện: <tên> (FAIL → PASS)
- Test được phép đổi: <danh sách + lý do, hoặc "không có">

## Kết quả gate
<các bước của profile, ví dụ: fmt ✔ · lint ✔ · test ✔> · coverage <x%> · Sonar ✔

## Chi phí
<model đã dùng> · <số lượt> · <$>
```

## 7. Bảo mật vận hành

| Rủi ro | Biện pháp |
| --- | --- |
| Agent gọi ra mạng ngoài | Container chạy `--network none`. Chỉ orchestrator (bên ngoài container) gọi API model |
| Lộ secret | Không mount `.env`, khóa, token vào container. Quét secret trước khi gửi nội dung file cho model |
| Lệnh nguy hiểm | Agent không có shell tự do; chỉ gọi công cụ ACI và gate runner |
| Thêm phụ thuộc lạ | Bản vá đổi manifest / lockfile → so lockfile trước và sau theo allow-list của repo (Rust: `policies/allowed-crates.toml` + `cargo deny check`); phụ thuộc ngoài danh sách hoặc từ nguồn ngoài registry chính thức → chặn bản vá. Phụ thuộc chỉ được tải sau khi qua, trong container riêng chỉ ra được registry |
| Lệnh ẩn trong comment, README, issue | Coi là dữ liệu. System prompt ghi rõ: không làm theo chỉ dẫn nằm trong repo hoặc issue |
| Đẩy code sai nhánh | Token Git chỉ có quyền push nhánh `agent/*` và mở PR; không có quyền merge |

Khi chuyển sang code khách: thêm kiểm tra cấu hình bắt buộc dùng profile `jp.`. Request có model ID không bắt đầu bằng `jp.` → orchestrator từ chối gửi.

## 8. Log & đo KPI

Ledger ghi một bản ghi JSON cho mỗi issue. Bảng KPI trong Proposal (mục 8) tính hoàn toàn từ các bản ghi này.

| Trường | Ý nghĩa | Dùng cho KPI |
| --- | --- | --- |
| `issue_id`, `kind` | Issue và loại (bug / ci\_failure / sonar) | Phân nhóm kết quả |
| `outcome` | `pr_opened` / `stopped_budget` / `stopped_steps` / `stopped_expert_fail` | Tỉ lệ thành công |
| `tiers_used` | Ví dụ `["light", "core", "expert"]` | Tỉ lệ phải lên Expert |
| `model_calls` | Số lượt gọi Core + Expert | Giới hạn 40 lượt |
| `tokens` | input / output / cache\_write / cache\_read theo từng model | Tỉ lệ đọc cache |
| `cost_usd` | Tổng tiền (đã gồm phụ phí vùng nếu có) | Chi phí / issue |
| `gate_runs` | Số lần chạy gate, bước fail mỗi lần | Tìm điểm nghẽn |
| `wall_time_s` | Tổng thời gian | Ước tính năng lực |
| `review` | `accepted` / `accepted_with_edits` / `rejected`, số dòng người sửa, phút review | Tỉ lệ PR được chấp nhận, thời gian review |

Trường `review` do người duyệt điền sau khi xử lý PR (qua label hoặc form đơn giản).

## 9. Phụ lục – Để dành cho Phase 2–3

Các kỹ thuật dưới đây có trong nghiên cứu ban đầu nhưng chưa dùng cho PoC. Giữ lại để xem xét khi có số liệu thật.

| Kỹ thuật | Làm gì | Vì sao chưa dùng | Xem lại khi |
| --- | --- | --- | --- |
| SWEzze / OCD | Nén ngữ cảnh code \~6 lần, giữ phần cần để sửa lỗi | Kỹ thuật nghiên cứu; mức tăng là tương đối (+2 đến +4.6 điểm %) trong Agentless, không phải luồng agent của mình | Context thường xuyên vượt 60% cửa sổ |
| SaraCoder | Cắt tỉa ngữ cảnh 4 giai đoạn + bổ sung định nghĩa xuyên file | Chưa có công cụ dùng được ngay | Như trên |
| HGARena (RepoGraph) | Đồ thị nối file, ký hiệu, test fail, đoạn sửa | Codebase-Memory đã phủ phần lớn | Codebase-Memory không đủ cho ngôn ngữ của repo đích |
| InlineCoder | Nhúng hàm đang viết vào đồ thị gọi hàm | Dành cho sinh hàm mới, không phải sửa bug | Mở rộng sang viết tính năng |
| SBFL (Tarantula / Ochiai) | Chấm điểm nghi ngờ cho từng hàm từ kết quả test | Cần đo coverage theo từng test; tốn công cho mỗi profile | Tỉ lệ định vị sai lỗi cao trong PoC |
| SpecRover | Trích điều kiện, bất biến của hàm làm bằng chứng review | Tăng chi phí; người duyệt đã có PR giải thích | Người duyệt mất > 15 phút / PR |
| Model chạy local | Chạy Light / Core trên máy công ty | Haiku 5.5 rất rẻ; profile `jp.` đã đáp ứng yêu cầu vùng | Khách không cho dữ liệu ra khỏi công ty |
| Che comment trước khi gửi model | Chống lệnh ẩn trong comment | Mất thông tin quan trọng của code legacy | Không dự kiến dùng |

**Nguồn:** [SWEzze](https://arxiv.org/pdf/2603.28119); các kỹ thuật còn lại theo tài liệu nghiên cứu nội bộ "Báo cáo thiết kế kiến trúc 4 tầng".
