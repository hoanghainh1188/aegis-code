# ADR 0005 – Ngưỡng KPI chốt sau Phase 1

- Ngày: 2026-10-24
- Trạng thái: Đã chấp nhận
- Người quyết định: @Nguyen Han Hoang Hai
- Thay thế một phần: Proposal §8 (bảng ngưỡng đạt PoC)

## Bối cảnh

Proposal §8 đặt các ngưỡng PoC là "đề xuất ban đầu", cần chốt lại với team sau khi chạy thử. [ADR 0002](0002-full-platform-mvp-as-checkpoint.md) coi kết quả PoC là mốc đo để điều chỉnh hướng: không đạt thì sửa phần lõi trước, và chốt lại ngưỡng theo số liệu thật.

Bộ thử 32 issue ([eval/README.md](../../eval/README.md)) được đo ba lần. Mỗi issue tính lượt chạy mới nhất:

| | #18 ([báo cáo](../measurements/2026-10-24-eval-kpi.md)) | #142 ([báo cáo](../measurements/2026-10-24-rust-rerun.md)) | #145 (PR #151) |
| --- | --- | --- | --- |
| Sửa lõi trước lượt đo | #127, #129, #133 | #136, #137, #140 | #144 |
| PR được chấp nhận, Python (scrapy, 14 issue) | 64% | — | — |
| PR được chấp nhận, Rust (typst, 18 issue) | 33% | 39% | 39% |
| PR được chấp nhận, tổng | 47% | 50% | 50% |
| PR làm vỡ test cũ | 0 | 0 | 0 |
| Chi phí / issue | $0.53 | $0.51 | $0.55 |
| Tỉ lệ đọc cache | 92% | 93% | 92% |

Qua ba vòng sửa lõi, Rust giữ ở mức 33–39%. Các sửa lõi đã đổi được cách agent làm việc: ở #145, cả 18 lượt đều chạy test tái hiện sớm, từ lượt 5–18. Nhưng tỉ lệ chấp nhận không vượt được 40%. Sáu issue typst hết 40 lượt ở cả ba lần đo. Mỗi issue chỉ chạy một lần mỗi đợt, nên chênh lệch 1–2 issue giữa các đợt nằm trong mức nhiễu.

## Quyết định

1. **Ngưỡng đóng Phase 1**, theo từng ngôn ngữ ([ADR 0004](0004-multi-language-profiles.md)):

   | KPI | Ngưỡng | Kết quả Phase 1 | |
   | --- | --- | --- | --- |
   | PR được chấp nhận (sửa ≤ 10 dòng), tổng | ≥ 40% | 50% | ✔ |
   | PR được chấp nhận, mỗi ngôn ngữ | ≥ 35% | Python 64%, Rust 39% | ✔ |
   | PR làm vỡ test cũ tới tay người | 0 | 0 | ✔ |
   | Chi phí / issue (cả ca fail) | ≤ $4 trung bình | $0.55 | ✔ |
   | Thời gian người duyệt / PR | ≤ 15 phút | 1,8 phút | ✔, xem hệ quả |
   | Tỉ lệ đọc cache | ≥ 70% | 92% | ✔ |
   | Tỉ lệ issue phải lên Expert | Theo dõi | 75% | |

2. **Phase 1 đóng mốc đo.** Dự án chuyển sang Phase 2, bắt đầu từ #22.
3. **40% cho mỗi ngôn ngữ là mục tiêu ở Phase 2**, không phải điều kiện để bắt đầu Phase 2. KPI theo ngôn ngữ được theo dõi liên tục trên dashboard (#28).
4. **Giữ trần $4 và 40 lượt gọi mỗi issue** như hiện tại (thiết kế §5.3). Chi phí thật chỉ khoảng 15% trần, nên trần lượt gọi là ràng buộc thật. Đổi trần lượt gọi thì đi qua issue riêng, kèm số đo.

## Hệ quả

- Proposal §8 dẫn tới ADR này cho ngưỡng đã chốt; §9 ghi Phase 1 đã đóng mốc đo.
- Các sửa lõi cho Rust được đề xuất ở #145 vẫn là việc nên làm, nhưng làm song song với Phase 2, không chặn nó. Cụ thể: thử trần 80 lượt cho typst, siết luật nhắc chạy test, và chạy mỗi issue hai lần khi đo.
- **Thời gian duyệt chưa đo được đáng tin.** Ở Phase 1, mỗi PR được duyệt khoảng 1,5–2 phút, nhiều số là ước lượng, và người duyệt biết trước kết quả hidden test. Tỉ lệ chấp nhận ở đây vì vậy gần với "qua gate và pass hidden test" hơn là với đánh giá đầy đủ của người. Ở Phase 2, giao diện người duyệt (#28) phải ghi thời gian duyệt thật và không hiện kết quả hidden test.
- **Số đo trên repo OSS.** typst và scrapy là repo công khai, model có thể đã thấy cả bản sửa. Proposal §2 chốt dùng repo nội bộ; khi có repo nội bộ, phải đo lại với cùng các ngưỡng này.
- Ngưỡng 35% cho mỗi ngôn ngữ áp dụng cho cả các profile mới của Phase 2 (TypeScript, Java, Go, C#) khi chúng được đo.

## Phương án đã cân nhắc

| Phương án | Lý do không chọn |
| --- | --- |
| Giữ 40% cho mỗi ngôn ngữ, tiếp tục sửa lõi cho tới khi Rust đạt | Ba vòng sửa lõi không đẩy Rust qua 39%; phần còn lại nằm trong mức nhiễu của một lần chạy, nên thêm vòng sửa chưa chắc đổi được kết luận, mà lại hoãn Phase 2 |
| Chỉ dùng ngưỡng tổng 40%, bỏ ngưỡng theo ngôn ngữ | Một ngôn ngữ tốt có thể che một ngôn ngữ kém; ADR 0004 yêu cầu báo KPI theo từng ngôn ngữ |
| Nâng trần lượt gọi lên 80 rồi đo lại trước khi chốt | Đáng làm, nhưng là thí nghiệm của Phase 2 (hệ quả ở trên), không phải điều kiện để chốt |
