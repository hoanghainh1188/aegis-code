# Đo KPI Kotlin trên bộ thử detekt (#626)

Ngày đo: 2026-11-23/24. Bộ thử: 5 issue `detekt/detekt` của [eval/issues](../../eval/issues) (`detekt-7333`, `detekt-7420`, `detekt-8545`, `detekt-9721`, `detekt-9780`), mỗi issue hai lượt, tổng 10 lượt. Bản dựng: từ `main` sau #650 (#649), chạy `aegis-local eval --no-pr`. Model và trần: Core `claude-sonnet-5-5`, leo thang lên Expert `claude-opus-5-5`, ngân sách $4.00 và mặc định 40 lượt gọi mỗi lượt. Tổng chi tiêu: **$4.41**.

"Được chấp nhận" ở đây vẫn là qua gate **và** qua test ẩn, chạy với `--no-pr`, không có người duyệt.

## Kết luận

**Kotlin chưa đạt ngưỡng 35% của [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md): 2/10 (20%).** Mẫu nhỏ (5 issue); 3 issue trượt vì test tái hiện hẹp hơn test upstream, không phải do language profile.

| KPI | Kotlin | Ngưỡng ADR 0005 |
| --- | --- | --- |
| Qua gate (sẽ mở PR) | 8/10 (80%) | – |
| Được chấp nhận (test ẩn qua) | **2/10 (20%)** | ≥ 35% → **chưa đạt** |
| Qua gate nhưng trượt test ẩn | 6/8 | – |
| Chi phí / lượt | $0.44 (tổng $4.41) | – |
| Chi phí / bản vá được chấp nhận | $2.21 | – |
| Số lượt gọi model / lượt | 21.3 (trung vị 18.5) | mặc định 40 |
| Lên Expert | 6/10 lượt | – |
| Thời gian gate | 447–500 s mỗi lần qua, cộng khoảng 220 s cho `repro_test` trượt | – |
| Thời gian thực tế (wall time) / lượt | 263–1356 s | – |

## Kết quả từng issue

"Lượt gọi" là số lượt gọi Core + Expert. Số hiệu lượt là `r1 / r2` hoặc `r3 / r4` của mỗi issue.

| Issue | Lượt | Kết quả | Lượt gọi | Chi phí | Expert | Test ẩn |
| --- | --- | --- | --- | --- | --- | --- |
| detekt-7333 | r1 / r2 | dừng: 40 lượt gọi | 40 / 40 | $1.00 / $0.94 | có | – |
| detekt-7420 | r3 / r4 | qua gate | 21 / 18 | $0.47 / $0.43 | có | **pass / pass** |
| detekt-8545 | r1 / r2 | qua gate | 20 / 17 | $0.48 / $0.44 | có | fail / fail |
| detekt-9721 | r1 / r2 | qua gate | 14 / 19 | $0.17 / $0.23 | không | fail / fail |
| detekt-9780 | r3 / r4 | qua gate | 11 / 13 | $0.12 / $0.13 | không | fail / fail |

## Phân tích

- **Pipeline hoạt động thông suốt từ đầu đến cuối.** Lượt chạy khô `--replay` của `detekt-9780` (bản sửa upstream, không gọi model) qua cả gate lẫn test ẩn. Do đó các ca trượt test ẩn không phải do harness gây ra.
- **`detekt-8545`, `detekt-9721` và `detekt-9780`** qua gate ở cả hai lượt, nhưng mỗi issue đều trượt đúng 2 test case ẩn. Trong mọi trường hợp, các test khác trong cùng class đều qua. Agent đã sửa đúng trường hợp được nêu trong báo cáo lỗi nhưng bỏ sót các biến thể mà bản sửa upstream xử lý:
  - `detekt-9780`: `internal constructor` của một `internal class` đi cùng `@PublishedApi`.
  - `detekt-9721`: một `Closeable` được trả về kèm theo hoặc dùng bên trong `use`.
  - `detekt-8545`: lời gọi `toList` theo sau một sequence.

  Test tái hiện của agent hẹp hơn test của upstream. Đây là điểm yếu đã biết của cách kiểm thử ưu tiên tái hiện (repro-first), không phải lỗi riêng của Kotlin.
- **`detekt-7333`** (`autoCorrect` không có `buildUponDefaultConfig`) trải qua nhiều lớp cấu hình của `detekt-core`. Cả hai lượt đều dùng hết 40 lượt gọi để đọc code và chưa từng tới gate.
- **`max_model_calls` ([ADR 0014](../adr/0014-per-language-call-cap.md)):** các lượt tới được gate dùng 11–21 lượt gọi. Chỉ `detekt-7333` chạm trần, và lúc đó vẫn đang dò code chứ chưa gần một bản sửa. Tăng trần cho profile Kotlin sẽ không đổi kết quả, nên giữ mức mặc định.
- Một lượt chạy đầu tiên (trước `--max-workers=2` và trước #649) trượt vì lý do môi trường (bộ nhớ, thiếu toolchain JDK 17, host bị chặn) và được loại ra. Các vấn đề đó dẫn tới #648 và #649, và 3 issue đã được thay thế (`7767`, `9468`, `9607` cần JDK 17 trong `build-logic`).
- `--no-pr`: chưa có fork của `detekt`, nên không có PR thật nào được mở. Kết quả `PrOpened` nghĩa là lượt chạy đã qua gate và tới bước mở PR.

## Lưu ý

- Tỉ lệ 20% nằm dưới ngưỡng 35% của [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md) cho Kotlin. Mẫu nhỏ (5 issue), và 3/5 issue trượt do độ phủ test tái hiện hẹp hơn upstream chứ không phải do language profile.
- `detekt/detekt` là mã nguồn mở, model có thể đã thấy bản sửa khi huấn luyện.

## Chạy lại

```bash
PATH=$PWD/.workspace/tools/cbm:$PATH target/release/aegis-local eval --config config/detekt.toml --set eval/issues --no-pr --only detekt-7420
```

Lặp cho từng issue `detekt` (`detekt-7333`, `detekt-7420`, `detekt-8545`, `detekt-9721`, `detekt-9780`).
