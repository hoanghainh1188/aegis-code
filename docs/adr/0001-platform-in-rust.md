# ADR 0001 – Viết platform bằng Rust, tự làm vòng lặp agent

- Ngày: 2026-10-17
- Trạng thái: Đã chấp nhận
- Người quyết định: @Nguyen Han Hoang Hai

## Bối cảnh

Bản Proposal và Thiết kế kỹ thuật ban đầu chọn Claude Agent SDK làm agent harness (dùng sẵn vòng lặp, công cụ đọc/sửa file, chạy lệnh) và chỉ tự làm phần còn thiếu.

Mục tiêu nay là xây platform đầy đủ, và team muốn dùng Rust — cùng ngôn ngữ với repo được bảo trì. Claude Agent SDK chỉ có bản Python và TypeScript; Anthropic không có SDK Rust chính thức.

Khi xét lại, phần Agent SDK thật sự đóng góp nhỏ hơn dự kiến, vì thiết kế đã tự định nghĩa:

- Bộ công cụ ACI riêng (Thiết kế §3.1), không dùng công cụ đọc/sửa file mặc định.
- Luật thu gọn lịch sử riêng (§3.3).
- Bố cục prompt và 4 điểm ngắt cache riêng (§4.1).
- Router, ledger, gate runner (§5, §6, §8).

Phần còn lại là vòng lặp tool-use, khoảng vài trăm dòng.

## Quyết định

1. Platform viết bằng Rust, tổ chức thành Cargo workspace, mỗi thành phần một crate (`crates/`).
2. Tự làm vòng lặp agent. Gọi model qua trait `ModelProvider` với hai bản cài:
   - Claude API — gọi HTTP trực tiếp, không phụ thuộc crate cộng đồng.
   - Bedrock — dùng AWS SDK for Rust chính thức.
   Kiểm tra "model ID phải bắt đầu bằng `jp.`" (Thiết kế §7) nằm trong lớp provider.
3. Bộ công cụ ACI viết thành MCP server (SDK Rust chính thức của MCP). Orchestrator gọi ACI và Codebase-Memory qua cùng một giao thức MCP.
4. Giữ cổng PoC của Proposal §9: thiết kế theo hướng platform đầy đủ, nhưng làm theo thứ tự provider → vòng lặp → ACI → gate → ledger, chạy bộ 30–50 issue trước khi làm phần Pilot (luồng ticket, dashboard).

## Phương án dự phòng

Nếu vòng lặp tự làm chậm tiến độ PoC: chương trình Rust gọi Claude Code ở chế độ headless (`claude -p`) như subprocess, gắn ACI qua MCP, ledger đọc usage từ output JSON. Chỉ dùng cho cấu hình PoC (Claude API); cấu hình code khách vẫn đi qua provider Bedrock.

## Hệ quả

- Tự chịu trách nhiệm phần vòng lặp, retry, streaming, đặt `cache_control`, thu gọn lịch sử.
- Kiểm soát hoàn toàn bố cục prompt và điểm ngắt cache → dễ đạt KPI tỉ lệ đọc cache ≥ 70%.
- Không bị chặn khi SDK Python/TS đổi API; nhưng phải tự theo dõi thay đổi của Messages API.
- Platform cùng ngôn ngữ với repo đích: team bảo trì dễ đóng góp, và có thể cho platform tự sửa chính nó.

## Phương án đã cân nhắc

| Phương án | Lý do không chọn |
| --- | --- |
| Python/TS + Agent SDK (thiết kế ban đầu) | Không phải ngôn ngữ team muốn; phần SDK đóng góp nhỏ vì ACI, cache, thu gọn đều tự định nghĩa |
| Kết hợp: orchestrator Python/TS, công cụ Rust | Hai ngôn ngữ, hai bộ build/test cho một platform nhỏ |
| Crate cộng đồng cho Claude API | Không chính thức, có thể chậm cập nhật tính năng cache/effort mới |
