# Thực nghiệm Golden Master và Ephemeral Database Sidecar (#454)

- Ngày đo: 2026-11-15
- Issue: #454 (liên quan: #147, #490, #491; [Design §6.2](../design/01-architecture-detail.md#62-golden-master-characterization-testing), [ADR 0011](../adr/0011-ams-direction.md), [ADR 0013](../adr/0013-ephemeral-database-sidecars.md))
- Máy đo: Apple M4 Pro (`arm64`, 12 CPU), Docker `29.9.3`, image `aegis-sandbox-python:latest` và `postgres:16-alpine`
- Bộ thử: [`eval/golden-master/`](../../eval/golden-master/README.md) chạy thực nghiệm qua [`crates/aegis-gate/tests/golden_master_sidecar_eval.rs`](../../crates/aegis-gate/tests/golden_master_sidecar_eval.rs)

## Phạm vi và phương pháp

Lệnh chạy:

```bash
AEGIS_REQUIRE_DOCKER=1 cargo test -p aegis-gate --test golden_master_sidecar_eval -- --nocapture --test-threads=1
```

Môi trường chưa cấu hình LLM API key nên lượt đo này không gọi model thật; chi phí token (`$`) khi agent tự sinh test Golden Master trên repo legacy ngoài sẽ được đo trong đợt chạy thử (#158). Bộ benchmark kiểm chứng phần thực thi của `aegis-gate` trên 8 ca thử ở 3 repo mẫu (tất cả đều có 0 unit test trên `base`):

1. **Repo Python thiếu test (`legacy-billing`, `gm-01`..`gm-04`):** chạy toàn bộ gate trong container `aegis-sandbox-python:latest` (`--network none`).
2. **Repo Rust thiếu test (`legacy-ledger`, `gm-05`..`gm-06`):** chạy gate với toolchain `cargo` trên host (`coverage = false`).
3. **Repo Python dùng PostgreSQL (`legacy-orders-db`, `gm-07`..`gm-08`):** chạy trong `aegis-sandbox-python:latest` nối tới sidecar `postgres:16-alpine` qua mạng bridge nội bộ `--internal` (`PGHOST=postgres:5432`), thu thập telemetry `SidecarReport` trực tiếp từ `GateReport.sidecars`.

Mỗi ca hồi quy ẩn (`gm-01`, `gm-02`, `gm-03`, `gm-05`, `gm-06`, `gm-07`) áp dụng cùng một bản vá đột biến (sửa đúng bug ở `repro_test` nhưng làm lệch hành vi cũ ở nhánh không liên quan) dưới hai chế độ: **Baseline** (chỉ có `repro_test`) và **Golden Master** (thêm bộ characterization test đã pass trên `base`), kèm bản vá sạch để kiểm chứng không báo lỗi giả.

## 1. Tỉ lệ phát hiện regression

| Ca thử | Ngôn ngữ / Sidecar | Baseline (chỉ repro) | Có Golden Master | Bản vá sạch |
| --- | --- | --- | --- | --- |
| `gm-01-shipping-threshold` | Python / Không | Lọt gate (Pass) | **Bị chặn** ở `test` | Pass |
| `gm-02-null-coupon` | Python / Không | Lọt gate (Pass) | **Bị chặn** ở `test` | Pass |
| `gm-03-exempt-tax` | Python / Không | Lọt gate (Pass) | **Bị chặn** ở `test` | Pass |
| `gm-05-proration-credit` | Rust / Không | Lọt gate (Pass) | **Bị chặn** ở `test` | Pass |
| `gm-06-jpy-rounding` | Rust / Không | Lọt gate (Pass) | **Bị chặn** ở `test` | Pass |
| `gm-07-idempotent-payment` | Python / `postgres:16-alpine` | Lọt gate (Pass) | **Bị chặn** ở `test` | — |
| **Tổng (6 ca đột biến)** | | **0/6 (0%) bắt được** | **6/6 (100%) bắt được** | **5/5 (0% dương tính giả)** |

Ngoài ra, ca cố ý đổi định dạng (`gm-04-invoice-format`, snapshot `tests/ref/invoice.txt` qua `update_references` trong Docker `aegis-sandbox-python`):
- Không khai báo `allowed_test_changes`: gate **chặn** ở `test` (`2627 ms`) với gợi ý khai báo `allowed_test_changes` / `allowed_reference_changes`.
- Có khai báo `allowed_test_changes = ["test_invoice_snapshot"]`: gate **Pass** (`3521 ms`), tự động cập nhật `tests/ref/invoice.txt` và ghi vào `GateReport.references`.

## 2. Thời gian gate và overhead sidecar

### Các ca không sidecar (`gm-01`..`gm-06`)

| Ca thử | Môi trường | Baseline (ms) | GM + đột biến bị chặn (ms) | GM + bản vá sạch (ms) | Chênh lệch sạch vs baseline |
| --- | --- | ---: | ---: | ---: | ---: |
| `gm-01-shipping-threshold` | Docker Python | 2246 | 3790 | 3217 | +971 ms |
| `gm-02-null-coupon` | Docker Python | 2557 | 4019 | 2070 | -487 ms |
| `gm-03-exempt-tax` | Docker Python | 2216 | 3088 | 2477 | +261 ms |
| `gm-05-proration-credit` | Host Rust | 2665 | 3474 | 3622 | +957 ms |
| `gm-06-jpy-rounding` | Host Rust | 3514 | 3545 | 3857 | +343 ms |

Nhận xét: trên bản vá sạch, overhead chạy thêm tập test Golden Master nằm trong ngưỡng dao động lịch container (`~0.3–1.0 s`). Khi bản vá đột biến thất bại ở `test`, gate tốn thêm ~`0.9–1.5 s` so với ca Pass nhỏ trên Python vì phải chạy lại suite trên `base` (`test-base`) và rerun test nghi ngờ để loại test flaky (#104).

### Các ca PostgreSQL sidecar (`gm-07`, `gm-08`)

1. **`gm-07-idempotent-payment`:**
   - Chạy với `--network none` (không sidecar): fail ngay ở `repro_test` trong **1435 ms** (không kết nối được `postgres`).
   - Có sidecar `postgres:16-alpine` (Baseline, chỉ `repro_test`): **6926 ms** (lọt gate dù UPSERT ghi đè đơn hàng cũ).
   - Có sidecar `postgres:16-alpine` (Golden Master bắt đột biến): **12318 ms** (lâu hơn vì bước `test` bị fail kích hoạt reset DB cho `test-base` và rerun).

2. **`gm-08-refund-balance` (chạy trọn tất cả các bước gate với `postgres:16-alpine`):**
   - Tổng thời gian trọn gate: **7752 ms** (`7.75 s`).
   - Chi tiết từng bước (`StepReport.duration`): `scope` 0 ms, `secrets` 0 ms, `dependencies` skipped (`NoDependencyChange`), `format` 657 ms, `lint` 441 ms, `typecheck` skipped (`NotInProfile`), `repro_test` 938 ms, `test` 454 ms, `coverage` 499 ms, `sonar` skipped (`NotConfigured`), `integrity` 0 ms (tổng thời gian chạy lệnh các bước: **2989 ms**).
   - Chi phí vòng đời sidecar đo bởi `GateReport.sidecars` (`SidecarReport`, 1 container `postgres:16-alpine`):
     - Khởi tạo mạng `--internal` + container + probe `pg_isready`: **1226 ms**
     - Reset `tmpfs` (`stop --time 5` + `start` + `pg_isready`, **3 lần** cho `repro_test-base`, `test`, `coverage`): **3123 ms** (trung bình **1041 ms/lần**)
     - Dọn container và mạng (`teardown`): **175 ms**
     - Tổng thời gian quản lý sidecar: **4524 ms** (**58.4%** tổng thời gian trên repo test siêu nhỏ; cố định ở mức ~4.5 s mỗi lần gate qua đủ `repro_test`, `test`, `coverage`, nên tỉ trọng sẽ nhỏ đi tuyến tính trên repo có bộ test chạy hàng phút).

Kiểm tra hồi quy bộ test sidecar gốc (`cargo test -p aegis-gate --test sidecar_gate`): 3/3 pass trong **9.36 s**, xác nhận `SidecarReport` (`services = 1`, `resets = 1`) và cả hai chế độ truy cập `PGHOST` nội mạng lẫn `localhost = true`.

## Kết luận

1. Quy trình Golden Master của Design §6.2 phát hiện **6/6** regression âm thầm mà gate chỉ có `repro_test` bỏ lọt (**0/6**), đồng thời giữ **0%** dương tính giả trên bản vá sạch và chặn đúng thay đổi output không khai báo trong `allowed_test_changes`.
2. Ephemeral DB sidecar cho phép repo tích hợp PostgreSQL chạy trọn vẹn mọi bước gate (kể cả `coverage`) trong mạng cô lập `--internal` không Internet, với chi phí vòng đời `~4.5 s` cho một container `postgres:16-alpine` qua 3 lần reset trạng thái sạch.
3. **Điểm còn chờ pilot (#158):** số liệu này đo trực tiếp trên engine gate bằng tập đột biến kiểm soát (chưa đo chi phí prompt/token USD và tỉ lệ agent tự viết test Golden Master đạt chất lượng ngoài thực tế).
