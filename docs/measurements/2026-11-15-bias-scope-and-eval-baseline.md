# Phạm vi thiên lệch (bias) và baseline đo trên bộ eval (#511)

Ngày lập: 2026-11-15. Tài liệu này ghi lại phạm vi các quyết định có nguy cơ thiên lệch theo [ADR 0016](../adr/0016-platform-bias-scope-and-measurement.md), số đo baseline hiện có trên bộ [eval/issues](../../eval/issues), khoảng trống độ phủ cho `ticket_language` / `ticket_source` / `triage`, và quy trình đo khi pilot [#158](../pilot/2026-11-team-pilot-plan.md) chạy.

**Trạng thái đóng issue `#511`:** ADR 0016, instrumentation trong ledger/lifecycle, và các bảng theo dõi trong `aegis report` + dashboard web đã hoàn tất. Báo cáo đo thiên lệch trên dữ liệu khách hàng thực tế vẫn đang **chờ dữ liệu từ pilot `#158`**, nên `#511` chưa đóng cho tới khi bổ sung được số liệu pilot đầu tiên.

## 1. Bộ eval hiện tại phủ được những chiều nào

Quét toàn bộ **52 issue** trong `eval/issues/*.toml` cho thấy phân bố thực tế như sau:

| Chiều | Phân bố trong 52 issue `eval/issues` | Độ dài title + body (ký tự: min / median / max) | `upstream_bound` đã gắn nhãn |
| --- | --- | --- | --- |
| `csharp` | 5 (`nsubstitute/NSubstitute`: 2, `App-vNext/Polly`: 2, `StackExchange/StackExchange.Redis`: 1) | 1.043 / 3.588 / 5.136 | 1 |
| `go` | 5 (`gin-gonic/gin`: 5) | 1.123 / 2.894 / 7.964 | 2 |
| `java` | 5 (`google/gson`: 5) | 509 / 1.047 / 3.903 | 2 |
| `python` | 14 (`scrapy/scrapy`: 14) | 164 / 1.573 / 12.808 | chưa phân loại (Phase 1) |
| `rust` | 18 (`typst/typst`: 18) | 643 / 1.733 / 3.575 | chưa phân loại (Phase 1) |
| `typescript` | 5 (`colinhacks/zod`: 5) | 649 / 1.604 / 3.128 | 2 |

Phân bố theo hai chiều mới của ADR 0016 trên cùng 52 issue:

- **`ticket_language`:** **52/52 là tiếng Anh (`en`)**, `0` tiếng Nhật (`ja`), `0` tiếng Việt (`vi`). Hai file có ký tự dấu đơn lẻ (`typst-8703.toml` có `ó`; `zod-6657.toml` có `ê`, `é`), bộ nhận diện `detect_ticket_language` giữ đúng về `en` mà không nhầm sang `vi`.
- **`ticket_source`:** **52/52 là GitHub public issue (`github`)**, `0` Jira / JSM / ServiceNow / Backlog / Sonar / CI.
- **Triage & diagnostic lane:** Bộ eval offline không đo shadow triage (`triage miss`), vì mọi issue trong `eval/issues` đều đã chọn sẵn là bug code cần vá.

## 2. Baseline chênh lệch theo ngôn ngữ lập trình từ các đợt đo đã công bố

Bảng dưới tổng hợp các số đo đã ghi nhận ở các báo cáo gần nhất (cần lưu ý khác bản dựng và trần lượt gọi giữa các đợt, nên dùng để chỉ ra điểm nghẽn hiện hữu chứ không thay thế cho một lần chạy đồng bộ trên cùng commit):

| Nhóm / Đợt đo | Số lượt (`runs`) | Tỉ lệ chấp nhận / qua test ẩn | Dừng giữa chừng (`stopped`) | Lên Expert | Chi phí TB / lượt | Ghi chú |
| --- | --- | --- | --- | --- | --- | --- |
| C# — [#426](2026-11-10-csharp-rerun-kpi.md) (trần 40, `4d39c8e`) | 10 (5 issue × 2) | 20% (2/10) nghiêm; 25% (2/8) bỏ `Polly-3086` | 0% (0/10 — 10/10 mở PR) | 40% (4/10) | $0.29 | Chưa đạt ngưỡng 35% (`low-accept` theo ADR 0016) |
| Java — [#425](2026-11-11-java-rerun-kpi.md) (trần 40, `adbc5aa`) | 10 (5 issue × 2) | 50% (5/10) nghiêm; 83% (5/6) bỏ 2 issue upstream | 10% (1/10 — `gson-1768` lượt 1 hết 40 lượt) | 60% (6/10) | $0.37 | Đạt ngưỡng 35% ở cả hai cách chấm |
| Rust `typst` — [#145 / #448](2026-11-14-typst-call-cap.md) | 18 | 39% (7/18) ở trần 40; **56% (10/18)** ở trần 80 | 39% (7/18) chạm trần 40 ở #145 | — | $0.78 (#448) | Nâng trần 80 giảm dừng oan cho typst ([ADR 0014](../adr/0014-per-language-call-cap.md)) |
| Rust `typst` subset 8 issue × 2 — [#156](2026-11-14-typst-call-cap.md) (trần 80) | 16 | 3/8 qua cả 2 lần; 5/8 trượt cả 2 lần | `typst-8703` tự dừng 2/2 | — | $0.84 (lần 2) | Chi phí ổn định dưới $2/lượt |

Ba tín hiệu thiên lệch kỹ thuật quan sát được ngay trên dữ liệu công khai:

1. **Thiên lệch theo độ phức tạp hệ sinh thái & test (`csharp` vs `java`):** C# dùng trung bình 17.1 lượt ở #426 với `$0.29/lượt` nhưng nhiều lượt gặp lỗi async/threading timeout ở test ẩn (`Polly-3286` chạy tới 1800s) hoặc sai kiểu exception (`StackExchange.Redis-2689`), làm tỉ lệ chấp nhận chỉ đạt `20–25%`, thấp hơn Java `30–58` điểm phần trăm (`> ACCEPT_GAP_ALERT` = `25 pp`).
2. **Thiên lệch trần lượt gọi (`rust` ở trần 40 vs 80):** Trước ADR 0014, `typst` dừng tới `7/18` (`39% > STOP_RATE_ALERT` = `30%`) ở trần 40 lượt, trong khi C# chỉ dùng trung bình `17.1` lượt và Java `18.5` lượt.
3. **Độ lệch cỡ repo (`N=1`–`18`):** Bộ eval mới phủ 8 repo (trong đó 5 ngôn ngữ chỉ có đúng 1 repo, riêng `csharp` có 3 repo với `1–2` issue mỗi repo), nên chưa tách được thiên lệch của ngôn ngữ khỏi đặc thù của từng repo.

## 3. Instrumentation đã thêm và cách dùng cho pilot `#158`

Các trường và chỉ số đã có sẵn trong code để khi pilot `#158` chạy chỉ cần đọc kết quả định kỳ:

1. **Ledger & Lifecycle:** Mỗi run trong `prepare_and_work` và `review_and_work` tự động ghi `LedgerEvent::TicketMeta` (`programming_language`, `ticket_language`, `ticket_source`, `repo`). Ledger cũ không có event này vẫn đọc bình thường.
2. **KPI dừng (`stopped`):** `Kpi` tách rõ `stopped_budget`, `stopped_steps`, `stopped_expert_fail` và tỉ lệ `Stopped without PR` trên `aegis report` lẫn dashboard `/`.
3. **Báo cáo CLI (`aegis report`):** Tự in thêm các bảng `## Bias check: By programming language (#511)`, `## Bias check: By ticket language (#511)`, `## Bias check: By ticket source (#511)`, và `## Bias check: By repo (#511)` kèm cột `triage miss` và `flags` (`low-accept`, `high-stop`, `high-cost`, `triage-miss`) khi nhóm có từ `5` lượt trở lên (`MIN_BIAS_SAMPLE = 5`).
4. **Dashboard web (`/`):** Bổ sung `Stopped without PR` vào phần tổng quan và bảng KPI, thêm 2 bảng `By ticket source` và `By ticket language`, cùng bảng `Bias investigation flags (#511)` khi có nhóm vượt ngưỡng.

### Kế hoạch bổ sung số liệu khi pilot `#158` có dữ liệu

Khi đợt pilot đầu tiên tích lũy đủ tối thiểu **5 ticket đã hoàn tất mỗi nhóm** ở ít nhất hai nhóm trong cùng một chiều (ví dụ `en` vs `ja`/`vi`, hoặc `github` vs `jira`/`jsm`/`backlog`), chạy:

```sh
aegis report --config config/pilot.toml
```

Rồi ghi thêm mục `## Kết quả pilot #158` vào file measurement mới hoặc trực tiếp cập nhật chỉ mục để khép vòng đo cho `#511`.
