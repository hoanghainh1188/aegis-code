# ADR 0002 – Xây platform đầy đủ, MVP/PoC là mốc đo KPI

- Ngày: 2026-10-18
- Trạng thái: Đã chấp nhận
- Người quyết định: @Nguyen Han Hoang Hai
- Thay thế một phần: [ADR 0001](0001-platform-in-rust.md), mục 4 (cổng PoC)

## Bối cảnh

Proposal ban đầu đặt PoC làm cổng "đi tiếp / dừng": không đạt ngưỡng KPI (Proposal §8) thì dừng cả dự án. ADR 0001 giữ nguyên cổng này.

Mục tiêu nay là xây một platform đầy đủ: chạy như dịch vụ, nối luồng ticket thật, phục vụ nhiều repo, có dashboard và phân quyền — không chỉ một PoC.

## Quyết định

1. Mục tiêu dự án là platform đầy đủ (Phase 0 → 3 trong Proposal §9).
2. Giữ MVP/PoC trên bộ 30–50 issue để đo KPI. Kết quả là **mốc đo để điều chỉnh hướng**, không phải cổng dừng dự án:
   - Đạt ngưỡng → tiếp tục theo lộ trình.
   - Không đạt → phân tích nguyên nhân từ ledger (định vị lỗi, chi phí, cache, gate) và ưu tiên sửa phần lõi trước khi mở rộng lớp dịch vụ. Ngưỡng và trần $4 được chốt lại theo số liệu thật.
3. Thứ tự làm giữ nguyên: phần lõi (provider → vòng lặp → ACI → gate → ledger → vòng đời issue) xong và đo được trước, rồi mới tới lớp dịch vụ (Phase 2) và mở rộng (Phase 3). Mọi tính năng platform đều dựa vào phần lõi.
4. Mỗi phase có milestone và issue riêng trên GitHub; tài liệu này là chuẩn cho lộ trình khi Proposal chưa kịp cập nhật.

## Hệ quả

- Lộ trình có thêm milestone `Phase 2 – Platform` và `Phase 3 – Mở rộng`, kèm issue.
- Kiến trúc lõi phải sẵn cho chạy nhiều issue song song, nhiều repo, lưu trạng thái bền — dù MVP chạy theo lô từ CLI. Cụ thể: không giữ trạng thái toàn cục, cấu hình theo repo, ledger ghi qua một interface có thể đổi sang DB.
- Kiến trúc lớp dịch vụ và các ràng buộc cụ thể cho phần lõi: [ADR 0003](0003-platform-service-architecture.md).
- Rủi ro "build tiếp khi lõi chưa tốt" được giảm bằng việc đo KPI bắt buộc ở cuối Phase 1 và theo dõi KPI liên tục trên dashboard ở Phase 2.

## Phương án đã cân nhắc

| Phương án | Lý do không chọn |
| --- | --- |
| Giữ PoC làm cổng dừng (thiết kế ban đầu) | Không khớp mục tiêu xây platform đầy đủ |
| Bỏ đo KPI, build thẳng platform | Không biết lõi có sửa được bug thật và chi phí bao nhiêu trước khi đầu tư lớp dịch vụ |
