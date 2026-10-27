# Đánh giá Thực nghiệm & Quyết định về SBFL (Tarantula / Ochiai) (#34)

- Ngày: 2026-10-27
- Người thực hiện: Antigravity & Harry
- Nhiệm vụ: Đánh giá khả năng áp dụng Spectrum-Based Fault Localization (Tarantula / Ochiai) dựa trên số liệu thực tế của PoC và cài đặt thuật toán (Thiết kế §9, Proposal §7, #34).

---

## 1. Bối cảnh & Điều kiện Tiên quyết

Theo [Thiết kế §9](../../docs/design/01-architecture-detail.md#9-phụ-lục--để-dành-cho-phase-23) và Issue #34:
> *"SBFL (Tarantula / Ochiai): Chấm điểm nghi ngờ cho từng hàm từ kết quả test. Cần đo coverage theo từng test; tốn công cho mỗi profile. Xem lại khi tỉ lệ định vị sai lỗi cao trong PoC."*

Điều kiện kích hoạt của kỹ thuật này là: **Tỉ lệ định vị lỗi sai cao trong thực tế**.

---

## 2. Số liệu Thực tế từ PoC (Phase 1 & Phase 2)

Dựa trên phân tích ledger thực tế tại [Báo cáo Định vị Lỗi](../measurements/2026-10-25-localization.md) và [KPI Codebase-Memory](../measurements/2026-10-26-codebase-memory-kpi.md):

| Tiêu chí | Số liệu thực tế trên tập Issue PoC |
| :--- | :---: |
| **Mở đúng file sửa lỗi trong 4 lượt gọi đầu** | **20 / 24 lượt (83.3%)** |
| **Chỉ sửa file khác với bản sửa chuẩn** | **1 / 24 lượt (4.1%)** |
| **Sửa đúng file nhưng thiếu file khác của bản sửa** | 6 / 24 lượt (25.0%) |
| **Hành vi gọi công cụ tìm kiếm cấu trúc** | Agent hầu như chỉ dùng `search_text` và `open`, không gọi `search_graph` hay `find_definition` vì stack trace từ test fail đã chỉ thẳng vào vị trí lỗi. |

### Nhận xét quan trọng:
1. **Định vị sai vị trí lỗi là rất hiếm:** Agent mở đúng file mục tiêu ngay từ những lượt đầu nhờ đọc trực tiếp thông báo lỗi và stack trace từ `run_tests` của ACI.
2. **Điểm nghẽn không nằm ở định vị hàm:** Tỉ lệ thất bại của các ca khó chủ yếu do:
   * Sửa thiếu các file phụ thuộc liên đới khi bản vá yêu cầu $\ge 3$ file.
   * Lỗi logic thuật toán phức tạp khi sinh bản sửa.

---

## 3. Phân tích Chi phí & Overhead Kỹ thuật của SBFL

Để tính toán ma trận SBFL (tập hợp các câu lệnh được thực thi bởi từng test riêng lẻ):

1. **Trên Rust (`typst`):**
   * Coverage thông thường (`cargo llvm-cov`) chạy gộp toàn bộ test suite và xuất 1 file LCOV duy nhất (mất ~1.5 – 2 phút).
   * Để đo **coverage theo từng test**: Cần chạy từng test case độc lập với biến môi trường instrumentation profile riêng (`LLVM_PROFILE_FILE=...`). Với hàng trăm test của typst, tổng thời gian chạy test tăng vọt lên **15 – 25 phút/lần** (tăng 8–10 lần), phá vỡ hoàn toàn ngưỡng cache 5 phút của vòng lặp agent.
2. **Trên Python (`scrapy`):**
   * `pytest-cov` mặc định tính gộp. Việc gắn trace per-test làm tăng thời gian chạy test lên 3–5 lần.
3. **Hiệu quả thực tế của công cụ gợi ý:**
   * Tương tự như thực nghiệm với `Codebase-Memory` (#185), khi đưa thêm công cụ đồ thị/định vị, agent trong 32 lượt chạy không gọi thêm công cụ mới vì kết quả test fail đã cung cấp đủ thông tin dòng code và file xảy ra ngoại lệ.

---

## 4. Cài đặt Kỹ thuật trong Crate `aegis-aci`

Crate `aegis-aci` đã được bổ sung module `sbfl` độc lập ([`crates/aegis-aci/src/sbfl.rs`](file:///Users/hoanghainh/Projects/aegis-code/crates/aegis-aci/src/sbfl.rs)) với đầy đủ tính năng:
* **Công thức Tarantula:**
  $$\text{Score}_{Tarantula}(e) = \frac{\frac{e_f}{N_{fail}}}{\frac{e_f}{N_{fail}} + \frac{e_p}{N_{pass}}}$$
* **Công thức Ochiai:**
  $$\text{Score}_{Ochiai}(e) = \frac{e_f}{\sqrt{N_{fail} \cdot (e_f + e_p)}}$$
* Cấu trúc `SbflMatrix` ghi nhận phổ thực thi test (`record_test`) và trả về bảng xếp hạng Top-N phần tử nghi ngờ (`rank`, `format_report`).
* Unit tests kiểm chứng độ chính xác so với chuẩn lý thuyết toán học.

---

## 5. Quyết định & Khuyến nghị (Tiêu chí "Xong khi")

* **Quyết định:**
  1. **Không bật SBFL mặc định trong chu trình sửa lỗi chuẩn của Agent:** Do tỉ lệ định vị sai file trong thực tế chỉ là 4.1%, trong khi chi phí đo coverage per-test làm chậm thời gian chạy test lên đến 10 lần.
  2. **Lưu giữ module `sbfl` trong `aegis-aci`:** Sẵn sàng kích hoạt hoặc sử dụng làm công cụ mở rộng khi triển khai trên các dự án legacy đặc thù (các hệ thống có test suite chạy nhanh nhưng lỗi silent không xuất hiện stack trace).
