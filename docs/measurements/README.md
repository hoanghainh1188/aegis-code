# Báo cáo đo đạc: mục lục

Mỗi file ở đây là một báo cáo theo ngày: một lượt đo, số liệu của nó, và quyết định rút ra. Báo cáo không được sửa lại khi có lượt đo mới; lượt đo mới là một báo cáo mới. Trang này cho biết số nào đang hiện hành và báo cáo nào đã được thay.

Ngưỡng KPI nằm ở [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md). Bộ thử và cách chấm nằm ở [eval/README.md](../../eval/README.md).

## KPI hiện hành theo ngôn ngữ

Chỉ số là tỉ lệ được chấp nhận. Ngưỡng là ≥ 35% cho mỗi ngôn ngữ và ≥ 40% tính chung.

| Ngôn ngữ (repo) | Tỉ lệ | Cách chấm | Lượt đo | Báo cáo |
| --- | --- | --- | --- | --- |
| Python (scrapy) | **64%** (9/14) | Người duyệt chấm PR | #18, 2026-10-24 | [eval-kpi](2026-10-24-eval-kpi.md) |
| Rust (typst) | **56%** (10/18) | Gate và test ẩn, trần 80 lượt gọi | #448, 2026-11-13 | [csharp-prompt-rules](2026-11-13-csharp-prompt-rules.md#rust-typst) |
| TypeScript (zod) | **100%** (6/6) | Gate và test ẩn, quy tắc #424 | #448, 2026-11-13 | [csharp-prompt-rules](2026-11-13-csharp-prompt-rules.md#typescript-zod-và-go-gin) |
| Go (gin) | **100%** (6/6) | Gate và test ẩn, quy tắc #424 | #448, 2026-11-13 | [csharp-prompt-rules](2026-11-13-csharp-prompt-rules.md#typescript-zod-và-go-gin) |
| Java (gson) | **83%** (5/6) | Gate và test ẩn, quy tắc #424 | #425, 2026-11-11 | [java-rerun-kpi](2026-11-11-java-rerun-kpi.md) |
| C# (Polly, NSubstitute, StackExchange.Redis) | **50%** (4/8) | Gate và test ẩn, quy tắc #424 | #448, 2026-11-13 | [csharp-prompt-rules](2026-11-13-csharp-prompt-rules.md) |

Đọc bảng này cần lưu ý:
- **Hai cách chấm khác nhau.** Chỉ Phase 1 (#18) có người duyệt chấm từng PR. Các lượt sau chạy với `--no-pr`: "được chấp nhận" là qua gate và qua test ẩn, không có người duyệt. Số liệu có người duyệt thật sẽ đến từ đợt chạy thử #158.
- **Quy tắc #424** bỏ khỏi phần chấm các issue mà test ẩn gắn với một lựa chọn riêng của upstream ([eval/README.md](../../eval/README.md#scoring-hidden-tests-bound-to-upstream-choices-424)). Chấm nghiêm thì Java là 50% (5/10) và C# là 40% (4/10).
- **Mẫu nhỏ.** Bốn profile mới có 5–6 issue mỗi ngôn ngữ, mỗi issue hai lượt. Một lượt đổi kết quả làm tỉ lệ đổi 12–17 điểm.
- **Rust ở trần 40 lượt** là 39% (7/18, #145). Trần 80 thành mặc định của profile Rust từ [ADR 0014](../adr/0014-per-language-call-cap.md).
- **Python ở #448** là 8/12 trên các lượt đo được, chạy như phép thử hồi quy cho prompt, với 2 issue bị loại vì lỗi môi trường.
- Mọi repo trong bộ thử đều công khai, nên model có thể đã thấy bản sửa khi huấn luyện (#157).

Các KPI còn lại ở Phase 1 ([eval-kpi](2026-10-24-eval-kpi.md)): không PR nào làm vỡ test cũ tới tay người duyệt, chi phí $0.53 mỗi issue, thời gian duyệt 1.7 phút, tỉ lệ đọc cache 92%.

## KPI và bộ thử

| Ngày | Báo cáo | Nội dung | Trạng thái |
| --- | --- | --- | --- |
| 10-22 | [eval-dry-run](2026-10-22-eval-dry-run.md) (#104) | Phát lại bản sửa thật qua toàn bộ pipeline, chưa có model | Bước chuẩn bị cho #18 |
| 10-23 | [eval-first-10](2026-10-23-eval-first-10.md) (#18) | 10 issue scrapy đầu với model thật | Thay bởi eval-kpi |
| 10-24 | [eval-kpi](2026-10-24-eval-kpi.md) (#18) | KPI Phase 1 trên 32 issue: Python 64%, Rust 33% | **Hiện hành cho Python**; Rust đã đo lại |
| 10-24 | [rust-rerun](2026-10-24-rust-rerun.md) (#142) | Rust sau các sửa lõi, một phần bộ thử | Thay bởi rust-rerun-18 |
| 10-24 | [rust-rerun-18](2026-10-24-rust-rerun-18.md) (#145) | Rust trên cả 18 issue: 39%, trần 40 lượt | Mốc so sánh của ADR 0005; số mới ở #448 |
| 10-29 | [new-profiles-dry-run](2026-10-29-new-profiles-dry-run.md) (#259) | Chạy khô bộ thử TypeScript, Go, Java, C# | Bước chuẩn bị cho new-profiles-kpi |
| 11-10 | [new-profiles-kpi](2026-11-10-new-profiles-kpi.md) (#259) | KPI lần đầu của bốn profile mới, 20 issue | Mốc so sánh; từng ngôn ngữ đã đo lại |
| 11-10 | [csharp-rerun-kpi](2026-11-10-csharp-rerun-kpi.md) (#426) | C# sau khi sửa gate: 25% | Thay bởi csharp-prompt-rules |
| 11-11 | [java-rerun-kpi](2026-11-11-java-rerun-kpi.md) (#425) | Java sau bước tự kiểm trong prompt: 83% | **Hiện hành cho Java** |
| 11-13 | [csharp-prompt-rules](2026-11-13-csharp-prompt-rules.md) (#448) | Hai quy tắc prompt cho C#, và hồi quy trên TypeScript, Go, Python, Rust | **Hiện hành cho C#, TypeScript, Go, Rust** |
| 11-14 | [typst-call-cap](2026-11-14-typst-call-cap.md) (#156) | Trần 80 lượt gọi trên 8 issue typst, đo hai lần | Cơ sở của ADR 0014 |

## Thí nghiệm trên lõi agent

| Ngày | Báo cáo | Kết luận |
| --- | --- | --- |
| 10-20 | [codebase-memory-accuracy](2026-10-20-codebase-memory-accuracy.md) (#3) | Độ chính xác của đồ thị gọi hàm trên repo đích |
| 10-25 | [localization](2026-10-25-localization.md) (#185) | Agent có định vị sai file không, theo ledger Phase 1 |
| 10-26 | [codebase-memory-kpi](2026-10-26-codebase-memory-kpi.md) (#185) | KPI sau khi bật Codebase-Memory; agent không gọi công cụ đồ thị lần nào |
| 10-26 | [graph-impact-note](2026-10-26-graph-impact-note.md) (#203) | Giữ Codebase-Memory, nhưng platform hỏi đồ thị thay cho agent |
| 10-27 | [sbfl-evaluation](2026-10-27-sbfl-evaluation.md) (#34) | SBFL (Tarantula, Ochiai): hoãn, không bật mặc định |
| 10-28 | [context-compression](2026-10-28-context-compression.md) (#35) | Nén context nâng cao: không làm, context chưa từng vượt 40% cửa sổ |
| 10-29 | [early-stop](2026-10-29-early-stop.md) (#273) | Dừng sớm lượt không tiến triển: không thêm, mọi quy tắc đơn giản đều làm mất PR tốt |

## Prompt injection

Mô hình đe doạ ở [ADR 0009](../adr/0009-prompt-injection-threat-model.md). Bộ ca thử ở [eval/injection](../../eval/injection/README.md).

| Ngày | Báo cáo | Kết luận |
| --- | --- | --- |
| 11-11 | [prompt-injection-eval](2026-11-11-prompt-injection-eval.md) (#361) | Lần đo đầu, payload trong issue, README, comment code và log CI: 0/18 lượt làm theo |
| 11-11 | [injection-report-rule](2026-11-11-injection-report-rule.md) (#443) | Sau quy tắc báo cáo: agent báo cáo ở cả 18 lượt, chi phí không tăng |
| 11-12 | [injection-doc](2026-11-12-injection-doc.md) (#441) | Payload trong file tài liệu agent đọc: không lượt nào làm theo |
| 11-12 | [injection-flags](2026-11-12-injection-flags.md) (#444) | Platform tự gắn cờ lệnh nhúng: bắt mọi payload agent đã đọc, 1 cờ trên gần 1 triệu dòng sạch |
| 11-12 | [injection-review-notes](2026-11-12-injection-review-notes.md) (#439) | Payload trong note review: agent làm yêu cầu hợp lệ và bỏ qua payload ở cả 6 lượt |

## Model, chi phí và nhà cung cấp

| Ngày | Báo cáo | Nội dung |
| --- | --- | --- |
| 10-22 | [claude-api-live](2026-10-22-claude-api-live.md) (#6) | Claude API thật: cache, TTL và tham số effort |
| 10-27 | [japan-inference-profiles](2026-10-27-japan-inference-profiles.md) (#19) | Profile suy luận tại Nhật trên AWS Bedrock và GCP Vertex AI |
| 10-27 | [local-model-cost-analysis](2026-10-27-local-model-cost-analysis.md) (#38) | Chi phí GPU so với Bedrock, và khuyến nghị model local |
| 11-09 | [bedrock-cache-ttl](2026-11-09-bedrock-cache-ttl.md) (#55) | Bedrock tách `cache_creation` theo TTL |

## Sandbox và triển khai

| Ngày | Báo cáo | Nội dung |
| --- | --- | --- |
| 10-20 | [build-test-target-repos](2026-10-20-build-test-target-repos.md) (#2) | Thời gian build và test của repo đích |
| 10-21 | [sandbox-test-target-repos](2026-10-21-sandbox-test-target-repos.md) (#5) | Test của repo đích trong sandbox không mạng |
| 10-29 | [k8s-gate-sandbox](2026-10-29-k8s-gate-sandbox.md) (#264) | Sandbox của gate trên Kubernetes, thử trên kind; cơ sở của [ADR 0008](../adr/0008-gate-sandbox-on-kubernetes.md) |
| 11-09 | [helm-chart-kind](2026-11-09-helm-chart-kind.md) (#281) | Helm chart kiểm trên kind |
| 11-09 | [helm-docker-proxy-kind](2026-11-09-helm-docker-proxy-kind.md) (#411) | `aegis-docker-proxy` trong worker Pod, kiểm trên kind; thiết kế ở [ADR 0010](../adr/0010-docker-socket-proxy.md) |

## Khi thêm báo cáo mới

1. Đặt tên `YYYY-MM-DD-<chủ đề>.md`, tiêu đề kết thúc bằng số issue.
2. Thêm một dòng vào bảng của chủ đề ở trên.
3. Nếu báo cáo thay số của một ngôn ngữ, sửa bảng "KPI hiện hành" và ghi "Thay bởi" ở dòng của báo cáo cũ.
