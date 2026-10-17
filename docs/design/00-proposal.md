# Agentic Code Maintenance Platform – Proposal

Oct 17, 2026 · @Nguyen Han Hoang Hai

Mục tiêu: xây platform đầy đủ ([ADR 0002](../adr/0002-full-platform-mvp-as-checkpoint.md)). Bắt đầu bằng MVP/PoC 6–8 tuần trên một repo bảo trì thật để đo KPI ở mục 8; kết quả dùng để điều chỉnh hướng, không phải cổng dừng. Platform viết bằng Rust, tự làm vòng lặp agent (xem [ADR 0001](../adr/0001-platform-in-rust.md)).

Chi tiết cách làm nằm ở tab Thiết kế kỹ thuật.

## 0. Có nên bắt đầu dự án không?

**Có. Mục tiêu là platform đầy đủ, nhưng làm phần lõi và đo KPI trên MVP/PoC trước khi xây lớp dịch vụ.**

**Vì sao nên làm:**

- Bảo trì là việc lặp lại, có test để kiểm chứng. Đây là loại việc agent làm tốt nhất.
- Chi phí thử rẻ: khoảng dưới $200 tiền API cho 50 issue (mục 9). Chi phí chính là công 1–2 người trong vài tuần.
- Kết quả PoC dùng được ngay làm bằng chứng khi đề xuất AI cho các dự án bảo trì (ví dụ AMS).

**Vì sao làm lõi và đo trước:**

- Các kỹ thuật nén context nâng cao (SWEzze, SaraCoder…) còn ở mức nghiên cứu, chưa đáng làm trước khi có số liệu thật.
- Platform viết bằng Rust, tự làm vòng lặp agent (ADR 0001); mọi tính năng platform đều dựa vào phần lõi, nên lõi phải xong và đo được trước.
- Rủi ro lớn nhất không nằm ở kỹ thuật. Nó nằm ở 2 câu hỏi: agent có sửa được bug thật của khách không, và khách có cho code ra API không.

**Nên tự làm phần nào:**

| Dùng sẵn | Tự làm (Rust) |
| --- | --- |
| Claude API / Bedrock (AWS SDK for Rust) | Vòng lặp agent, đặt điểm ngắt cache, thu gọn lịch sử |
| Codebase-Memory MCP, rust-analyzer, tree-sitter | Bộ công cụ ACI (MCP server) |
| Docker, image CI, Sonar | Luật leo thang model + trần $4 / 20 bước |
|  | Cổng kiểm chứng: test Docker + Sonar + người duyệt |
|  | Kết nối luồng ticket/PR của khách |
|  | Đo KPI và chi phí |

**Nếu MVP không đạt ngưỡng ở mục 8:** không dừng dự án, nhưng không mở rộng lớp dịch vụ khi lõi chưa tốt. Phân tích ledger để tìm điểm nghẽn (định vị lỗi, chi phí, cache, gate), sửa lõi, đo lại (ADR 0002).

**Việc cần làm tuần tới:**

- [ ] Chọn 1 repo bảo trì có test chạy tự động.
- [ ] Hỏi khách về việc gửi code ra API.
- [ ] Gom 30–50 issue đã đóng để làm bộ thử.

**Đã chốt cho PoC (17/10):**

| Mục | Quyết định | Ảnh hưởng |
| --- | --- | --- |
| Repo | Repo nội bộ, Rust, > 100k dòng | Dùng Codebase-Memory MCP làm chỉ mục (mục 3) |
| Test | Có, chạy trên CI | Đủ điều kiện làm PoC ngay; Phase 0 rút ngắn |
| Thời gian build | Chưa đo | Đo ở Phase 0; quyết định cache 5 phút hay 1 giờ |
| Dữ liệu | Khách chỉ cho xử lý ở Nhật | PoC trên repo nội bộ nên không vướng; khi dùng code khách → profile `jp.` (mục 7) |
| Sonar | Chuẩn bị cả Server và Cloud | Cả hai hỗ trợ Rust qua Clippy |
| Người đọc | Chỉ nội bộ | Giữ tiếng Việt, chưa cần bản tiếng Nhật |
| Ngôn ngữ platform | Rust, tự làm vòng lặp agent | Không dùng Claude Agent SDK; xem [ADR 0001](../adr/0001-platform-in-rust.md) |

## 1. Mục tiêu & phạm vi

Hệ thống nhận một issue bảo trì và trả về một Pull Request đã qua test, chờ người duyệt. Agent không tự merge.

**3 mục tiêu đo được:**

1. **Đúng:** tỉ lệ PR được chấp nhận ≥ 40% trên bộ issue thử nghiệm (xem mục 8).
2. **Rẻ:** chi phí trung bình ≤ $4 cho mỗi issue, kể cả ca thất bại.
3. **An toàn:** 0 PR làm vỡ test đang có được đưa tới người duyệt.

**Đầu vào:** ticket bug (Jira/Backlog), lỗi CI, cảnh báo Sonar.

**Đầu ra:** một nhánh riêng + PR, kèm giải thích từng đoạn sửa và kết quả test.

**Không làm trong giai đoạn này:**

- Tính năng mới, refactor lớn, sửa nhiều service cùng lúc.
- Tự merge vào nhánh chính.
- Repo không có cách chạy test tự động.

## 2. Kiến trúc tổng thể

Hệ thống gồm 4 tầng, mỗi tầng có luật rõ ràng. Tầng 2 không phải một bước riêng: nó áp dụng cho mọi lần gọi model.

&#91;embedded content: luồng một issue · 4 tầng, 1 nhánh leo thang, 1 điểm dừng\]

Issue chạy thẳng xuống. Fail ở Tầng 4 thì Core sửa lại; fail nhiều thì leo lên Expert; vẫn fail thì dừng và giao người.

## 3. Tầng 1 – Lấy đúng ngữ cảnh

Chỉ đưa cho model đúng phần code cần, không nạp cả repo. Giai đoạn PoC dùng 2 thứ, không hơn.

**a. Bộ công cụ cho agent (ACI – giao diện agent dùng để thao tác máy):**

- Tìm hàm/class theo cấu trúc code (AST), không dùng grep thô.
- Mỗi lần tìm trả tối đa 50 kết quả. Quá 50 thì bắt agent thu hẹp.
- Mở file chỉ thấy 100 dòng một lần, có số dòng. Thử nghiệm của SWE-agent cho thấy 100 dòng tốt hơn cả 30 dòng lẫn cả file.
- Lệnh sửa phải qua linter. Sai cú pháp thì từ chối ngay, không ghi vào file.

**b. Một bộ chỉ mục repo (chọn 1 trong 2):**

| Lựa chọn | Đầu ra | Hợp khi |
| --- | --- | --- |
| Aider RepoMap | Bản đồ chữ ký hàm/class, giới hạn theo token | Repo vừa, muốn đơn giản |
| Codebase-Memory MCP | Đồ thị gọi hàm lưu SQLite, truy vấn qua MCP | Repo lớn, nhiều module |

Lưu ý: con số 1–2k token chỉ là của phần bản đồ. Cả request vẫn có system prompt, tool, code và lịch sử, thường vài chục nghìn token.

**Với Rust:** Tree-sitter có sẵn grammar Rust, nên cả RepoMap lẫn công cụ tìm theo AST đều dùng được. Có thể cho agent gọi `rust-analyzer` để tra định nghĩa và chỗ dùng chính xác hơn.

**Chọn cho PoC: Codebase-Memory MCP.** Repo > 100k dòng thì bản đồ RepoMap 1–2k token không đủ phủ. Truy vấn đồ thị gọi hàm qua MCP hợp hơn.

**Bỏ khỏi phạm vi PoC:** SWEzze/OCD, SaraCoder, HGARena, InlineCoder. Đây là kỹ thuật nghiên cứu, chưa có công cụ dùng được ngay. InlineCoder còn dành cho sinh hàm mới, không phải sửa bug. Xem lại ở Phase 3 nếu cần.

## 4. Tầng 2 – Prompt caching

Phần đầu prompt giống nhau qua các lượt thì chỉ trả 5%–10% giá input. Đây là cách giảm chi phí rẻ và chắc nhất.

**Thứ tự prompt (cố định → thay đổi):**

1. Tool definitions.
2. System prompt + quy ước code của dự án.
3. Bản đồ repo (từ Tầng 1).
4. Issue + lịch sử các bước.
5. Kết quả tool mới nhất, log lỗi.

**Bảng giá hiện tại ($ / 1 triệu token):**

| Model | Input | Output | Ghi cache 5 phút | Ghi cache 1 giờ | Đọc cache |
| --- | --- | --- | --- | --- | --- |
| Fable 5.1 | 10 | 50 | 12.50 | 20 | 0.25 (2.5%) |
| Opus 5.5 | 4 | 20 | 5 | 8 | 0.20 (5%) |
| Sonnet 5.5 | 2 | 10 | 2.50 | 4 | 0.10 (5%) |
| Haiku 5.5 (prompt ≤ 100k) | 0.10 | 0.50 | 0.125 | 0.20 | 0.01 (10%) |

Nguồn: [Claude pricing](https://platform.claude.com/docs/en/about-claude/pricing). Haiku 5.5 tính giá cao hơn khi prompt trên 100k token.

**Cách bật:** dùng cache tự động (một trường `cache_control` ở cấp request). Hệ thống tự dời điểm ngắt khi hội thoại dài ra. Chỉ đặt điểm ngắt thủ công khi cần kiểm soát kỹ.

**Hòa vốn:** cache 5 phút có lời từ lần đọc thứ 1. Cache 1 giờ có lời từ lần đọc thứ 2. Mỗi lần đọc trúng cache cũng làm mới thời hạn.

**Đã xác nhận trên docs (17/10):**

- Ngưỡng tối thiểu để cache: 512 token cho Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5. Prompt ngắn hơn thì không cache và không báo lỗi.
- Thời hạn cache tính từ lúc **bắt đầu** request. Thời gian stream response cũng bị trừ vào. Response chạy 4 phút thì lượt sau phải bắt đầu trong khoảng 1 phút.
- Tối đa 4 điểm ngắt; cache tự động chiếm 1 trong 4.
- Kiểm tra cache có chạy không: xem `cache_creation_input_tokens` và `cache_read_input_tokens` trong response. Cả hai bằng 0 là không cache.

**Thu gọn lịch sử – xử lý xung đột với cache:**

- Thu gọn lượt cũ làm đổi phần đầu prompt, nên mất cache.
- Vì vậy chỉ thu gọn **theo đợt**: khi context vượt ngưỡng (ví dụ 60% cửa sổ), gom một lần, chấp nhận ghi cache lại một lần.
- Không thu gọn sau mỗi lượt.

**Những lỗi làm vỡ cache âm thầm:**

- Chèn ngày giờ, session ID vào system prompt.
- Serialize JSON không cố định thứ tự key.
- Đổi danh sách tool giữa chừng.

**Lưu ý khi ước tính:** model từ 4.7 trở đi dùng tokenizer mới, ra nhiều hơn khoảng 30% token cho cùng một đoạn văn bản. Khi ước tính token, cộng thêm khoảng 30%.

## 5. Tầng 3 – Chọn model

Mọi issue bắt đầu ở tầng Core. Chỉ leo lên Expert khi có lý do cụ thể.

| Tầng | Model gợi ý | Việc làm |
| --- | --- | --- |
| Light | Haiku 5.5 | Phân loại issue, tóm tắt log, cắt stack trace lặp, viết commit message |
| Core | Sonnet 5.5 | Tìm vị trí lỗi, viết test tái hiện, viết bản vá |
| Expert | Opus 5.5 (Fable 5.1 cho ca rất khó) | Tìm nguyên nhân gốc, sửa nhiều file, xét ảnh hưởng tới module khác |

**Hai cấu hình theo nơi chạy:**

| Tầng | PoC (repo nội bộ) | Code khách (chỉ ở Nhật, Bedrock `jp.`) |
| --- | --- | --- |
| Light | Haiku 5.5 | Haiku 5.5 |
| Core | Sonnet 5.5 | Opus 5.5, effort thấp |
| Expert | Opus 5.5 | Opus 5.5, effort cao |

**Luật leo thang Core → Expert (gặp 1 trong 3):**

1. Lệnh sửa bị linter từ chối 2 lần liên tiếp.
2. Test tái hiện vẫn fail sau 2 bản vá liên tiếp.
3. Lỗi trải trên ≥ 3 file.

**Luật dừng (giao cho người):**

- Expert thử 2 lần vẫn fail.
- Hoặc vượt 20 bước.
- Hoặc vượt $4 cho issue đó.

Lý do có trần: ca thành công thường xong nhanh và rẻ, ca thất bại thì kéo dài và tốn gấp đôi. Dừng sớm là cách tiết kiệm lớn nhất.

**Về model chạy local:** Haiku 5.5 chỉ $0.10/1M input, nên model local khó rẻ hơn khi tính cả GPU. Chỉ cân nhắc local khi khách không cho code ra ngoài (xem mục 7).

## 6. Tầng 4 – Chặn lỗi hồi quy (Regression-Guarded)

Bản vá chỉ tới tay người duyệt khi qua đủ 4 cổng dưới đây. Lưu ý: test chỉ bắt được lỗi mà test có phủ tới, nên không cam kết "không có lỗi hồi quy".

1. **Linter khi sửa:** sai cú pháp thì từ chối ngay (đã nêu ở Tầng 1).
2. **Test động (chạy trong Docker, không có mạng):**
   - Test tái hiện bug: phải chuyển từ FAIL sang PASS.
   - Toàn bộ test đang có: phải PASS như trước.
3. **Sonar Quality Gate:** dùng mặc định "Sonar way" cho code mới — coverage ≥ 80%, trùng lặp ≤ 3%, không lỗi mới, hotspot bảo mật đã xem hết.
4. **Người duyệt:** agent mở PR, kèm giải thích từng đoạn sửa. Người merge.

**Cấu hình cho Rust:**

- Linter: `cargo clippy` + `rustfmt --check`. Lỗi biên dịch cũng chặn bản vá ngay.
- Test: `cargo test` trong Docker, dùng lại image CI hiện có.
- Coverage: `cargo llvm-cov` xuất LCOV cho Sonar.
- Sonar: cả SonarQube Server và SonarQube Cloud đều hỗ trợ Rust qua Clippy, nhập coverage dạng LCOV hoặc Cobertura.
- Build Rust chậm: phải cache thư mục `target` và registry giữa các lần chạy. Nếu không, mỗi vòng sửa có thể tốn vài phút và vượt thời hạn cache 5 phút.
- Thời gian build chưa đo. Đo ở Phase 0: dưới 3 phút dùng cache 5 phút; từ 3 phút trở lên dùng cache 1 giờ. Nếu trên 10 phút, chỉ chạy test của crate bị sửa (`cargo test -p <crate>`), chạy toàn bộ ở bước cuối.

**Với code legacy chưa có test (Golden Master):**

- Trước khi sửa, agent viết test ghi lại hành vi hiện tại, chạy trên code gốc để chắc là PASS.
- Bẫy cần tránh: test này cũng ghi lại chính con bug. Sửa xong nó sẽ fail.
- Cách xử lý: agent phải đánh dấu rõ test nào "được phép đổi kết quả" và lý do. Người duyệt xác nhận.

**Khi thất bại:** agent làm trên nhánh hoặc worktree riêng. Fail thì xóa nhánh. Không cần cơ chế rollback riêng.

**Định vị lỗi (SBFL):** chạy test, chấm điểm nghi ngờ cho từng hàm. Hữu ích khi repo có test tốt. Để ở Phase 3, hoặc sớm hơn nếu MVP cho thấy định vị lỗi sai nhiều; không bắt buộc cho MVP.

## 7. Bảo mật & dữ liệu khách hàng

Code của khách sẽ được gửi tới API model. Việc này phải được khách đồng ý bằng văn bản trước khi chạy PoC.

**Cần chốt với khách:**

- Có cho phép gửi code ra API không? Có cần hợp đồng/điều khoản riêng không?
- Có yêu cầu dữ liệu ở vùng cụ thể không?

**Lựa chọn về vùng xử lý:**

| Cách gọi | Ghi chú | Phụ phí |
| --- | --- | --- |
| Claude API, mặc định (global) | Đơn giản nhất | 0% |
| Claude API, chỉ xử lý ở Mỹ | Tham số `inference_geo` | +10% |
| Bedrock / Google Cloud, endpoint theo vùng | Chọn vùng gần khách hơn, ví dụ Nhật — cần kiểm tra model có sẵn ở vùng đó | +10% |

**Yêu cầu "chỉ xử lý ở Nhật" — đã có hướng:**

- **PoC:** chạy trên repo nội bộ, không phải code khách. Không vướng yêu cầu vùng, dùng model nào cũng được.
- **Khi chạy trên code khách:** gọi Bedrock từ Tokyo bằng profile `jp.`. Request chỉ được xử lý ở Tokyo hoặc Osaka.

| Model | Profile Nhật (`jp.`) trên Bedrock | Dùng cho tầng |
| --- | --- | --- |
| Haiku 5.5 | Có | Light |
| Sonnet 5.5 | **Chưa có** (chỉ global) | — |
| Opus 5.5 | Có | Core + Expert |

- Hệ quả: khi chạy với code khách, tầng Core phải dùng Opus 5.5 thay Sonnet 5.5. Giá input/output gấp đôi Sonnet, nên trần $4/issue cần đo lại.
- Endpoint theo vùng trên Bedrock có phụ phí khoảng 10% so với global.
- Prompt caching trên Bedrock: dùng điểm ngắt thủ công nếu cache tự động không hỗ trợ; ngưỡng token tối thiểu theo docs Bedrock.
- Khi Sonnet 5.5 có profile `jp.`, chuyển Core về Sonnet để giảm chi phí.

**Biện pháp kỹ thuật:**

- Sandbox chạy test không có mạng ra ngoài.
- Không đưa secret, file `.env`, dữ liệu thật vào repo agent đọc.
- Coi mọi nội dung trong repo (comment, README, ticket) là dữ liệu không tin cậy. Agent không làm theo lệnh nằm trong đó.
- Chỉ cho phép package trong allow-list. Package lạ → chặn bản vá.
- Giữ nguyên comment khi gửi cho model. Với code legacy, comment thường là tài liệu duy nhất.

## 8. KPI & cách đo

Đo trên repo thật của mình, không dựa vào SWE-bench. Số SWE-bench công khai khác bối cảnh dự án thật, và cũ đi rất nhanh.

**Bộ thử nghiệm:** 30–50 issue đã đóng trong 6 tháng gần nhất. Đã biết bản sửa đúng của người, nên so được.

| KPI | Cách đo | Ngưỡng đạt PoC |
| --- | --- | --- |
| Tỉ lệ PR được chấp nhận | Người duyệt chấp nhận, sửa ≤ 10 dòng | ≥ 40% |
| PR làm vỡ test cũ tới tay người | Đếm | 0 |
| Chi phí / issue (cả ca fail) | Usage API | ≤ $4 trung bình |
| Thời gian người duyệt / PR | Tự ghi | ≤ 15 phút |
| Tỉ lệ đọc cache | `cache_read_input_tokens` / tổng input | ≥ 70% |
| Tỉ lệ issue phải lên Expert | Log định tuyến | Theo dõi, chưa đặt ngưỡng |

Các ngưỡng trên là đề xuất ban đầu. Nên chốt lại với team sau 1 tuần chạy thử.

## 9. Lộ trình triển khai

Bốn phase. Mỗi phase có mốc đo; mốc không đạt thì quay lại sửa phần lõi trước khi đi tiếp, không dừng dự án (ADR 0002).

| Phase | Thời gian | Làm gì | Mốc đo |
| --- | --- | --- | --- |
| 0. Chuẩn bị | 1–2 tuần | Chọn repo, xin đồng ý của khách, dựng Docker chạy test, gom bộ 30–50 issue | Khách đồng ý bằng văn bản; test chạy được tự động |
| 1. MVP / PoC | 4–6 tuần | Platform Rust: provider → vòng lặp → ACI → gate → ledger, Tầng 2, 3, 4 tối thiểu; chạy bộ issue | Đạt các ngưỡng ở mục 8 |
| 2. Platform | 2–3 tháng | Chạy như dịch vụ (hàng đợi, worker, lưu trạng thái bền), nối luồng ticket thật (Jira/Backlog, GitHub/GitLab, CI, Sonar), nhiều repo, dashboard chi phí/KPI, phân quyền + audit, triển khai | Team dùng hằng ngày; chi phí/tháng trong ngân sách; KPI giữ được như MVP |
| 3. Mở rộng | Sau Phase 2 | SBFL, nén context nâng cao, SpecRover, thêm ngôn ngữ ngoài Rust, model local | Có ROI rõ ràng |

**Nhân lực gợi ý cho MVP:** 1 tech lead (bán thời gian) + 1–2 dev. Không cần team riêng.

**Ước tính chi phí API cho PoC:** 50 issue × tối đa $4 ≈ dưới $200, chưa tính chạy lại. Chi phí chính là công người.

## 10. Rủi ro & câu hỏi mở

| Rủi ro | Cách giảm |
| --- | --- |
| Khách không cho gửi code ra API | Hỏi ở Phase 0; phương án: endpoint theo vùng hoặc model local |
| Repo không chạy test tự động được | Chọn repo khác cho PoC; không cố gượng |
| Agent sửa đi sửa lại, đốt tiền | Trần $4 + 20 bước + luật dừng ở mục 5 |
| Xây lớp dịch vụ khi lõi chưa tốt | Đo KPI bắt buộc cuối Phase 1; theo dõi KPI liên tục trên dashboard ở Phase 2 (ADR 0002) |
| Golden Master khóa luôn con bug | Đánh dấu test "được phép đổi", người duyệt xác nhận |
| Cache miss do prompt thay đổi | Cố định thứ tự; theo dõi tỉ lệ đọc cache (mục 8) |

**Câu hỏi mở — trạng thái ngày 17/10:**

| Câu hỏi | Trả lời | Nguồn |
| --- | --- | --- |
| Repo nào cho PoC? | Repo **nội bộ**, Rust, > 100k dòng, test chạy trên CI | Bạn |
| Khách có cho code ra API? | Chỉ khi xử lý ở Nhật. Khách chấp nhận PoC trên repo nội bộ | Bạn |
| Có endpoint ở Nhật cho dòng 5.5? | Bedrock có profile `jp.` cho **Opus 5.5** và **Haiku 5.5** (Tokyo/Osaka). **Sonnet 5.5 chỉ có global.** Foundry chưa có vùng Nhật; Vertex chưa xác nhận | [Bedrock – Sonnet 5.5](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-5-5.html), [Opus 5.5](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-opus-5-5.html), [Haiku 5.5](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-haiku-5-5.html) |
| Ngưỡng token tối thiểu để cache? | 512 cho Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5. Sonnet 5 là 1,024. Bedrock có ngưỡng riêng | [Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) |
| Thời hạn cache tính từ đâu? | Từ lúc **bắt đầu** request ghi/đọc cache. Mỗi lần đọc trúng làm mới hạn. Response stream 4 phút thì lượt sau phải bắt đầu trong khoảng 1 phút | [Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) |
| SWEzze "5.0% → 9.2%" là gì? | Là mức tăng **tương đối** so với không nén, tức +2.0 đến +4.6 điểm %. Tỉ lệ giải quyết tuyệt đối 42%–58% | [Paper SWEzze](https://arxiv.org/pdf/2603.28119) |
| SonarQube loại nào? | Chuẩn bị cho cả Server và Cloud. Cả hai đều hỗ trợ Rust | Bạn |

**Còn mở:**

- [ ] Đo thời gian `cargo build` + `cargo test` trên CI (Phase 0). Dưới 3 phút → cache 5 phút; từ 3 phút trở lên → cache 1 giờ.
- [ ] Kiểm tra ngưỡng cache tối thiểu riêng của Bedrock cho Opus 5.5 và Haiku 5.5.
- [ ] Theo dõi khi nào Sonnet 5.5 có profile `jp.` (gọi `GetInferenceProfile` từ Tokyo).
- [ ] Kiểm tra Vertex AI có Opus / Sonnet 5.5 ở `asia-northeast1` không.

## 11. Nguồn tham khảo

- [Claude API – Pricing](https://platform.claude.com/docs/en/about-claude/pricing) — giá model, hệ số cache, cache tự động, tokenizer mới, phụ phí theo vùng.
- [Claude – Prompt caching](https://docs.claude.com/en/docs/build-with-claude/prompt-caching) — thứ tự tools → system → messages.
- Báo cáo thiết kế kiến trúc 4 tầng (số liệu SWE-agent, ACI, luật leo thang, trần $4).

* [Amazon Bedrock で利用可能になった Claude Sonnet 5.5 を試してみた – Classmethod](https://dev.classmethod.jp/articles/bedrock-claude-sonnet-5-5/) — Sonnet 5.5 trên Bedrock chỉ có profile global, chưa có profile Nhật (29/9/2026).
* [Introducing Bedrock cross-Region inference for Claude Sonnet 4.5 and Haiku 4.5 in Japan and Australia](https://aihub.hkuspace.hku.hk/2025/10/31/introducing-amazon-bedrock-cross-region-inference-for-claude-sonnet-4-5-and-haiku-4-5-in-japan-and-australia/) — dòng 4.5 xử lý trong Nhật.
* [SonarQube Server – Rust](https://docs.sonarsource.com/sonarqube-server/2026.4/analyzing-source-code/languages/rust) — Rust qua Clippy, coverage LCOV/Cobertura.

- [Claude – Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) — ngưỡng 512 token, thời hạn tính từ lúc bắt đầu request, 4 điểm ngắt.
- Bedrock model cards: [Sonnet 5.5](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-5-5.html) (chỉ global ở Tokyo), [Opus 5.5](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-opus-5-5.html) và [Haiku 5.5](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-haiku-5-5.html) (có profile `jp.`).
- [Compressing Code Context for LLM-based Issue Resolution (SWEzze)](https://arxiv.org/pdf/2603.28119) — 5.0%–9.2% là mức tăng tương đối.
- [Claude Opus 5.5 – Model availability](https://modelavailability.com/models/anthropic/claude-opus-5-5) — Foundry chưa có vùng Nhật.
