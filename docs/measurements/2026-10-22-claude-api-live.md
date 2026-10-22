# Claude API thật: cache, TTL và tham số effort (#6)

Ngày đo: 2026-10-22. Gọi `https://api.anthropic.com/v1/messages` với `anthropic-version: 2023-06-01`, qua `aegis-provider` (`ClaudeApi`). Test: `crates/aegis-provider/tests/live_api.rs`. Test chỉ chạy khi đặt `AEGIS_LIVE_API=1` và có `ANTHROPIC_API_KEY`, nên CI và `cargo test` thường không tốn tiền:

```bash
AEGIS_LIVE_API=1 cargo test -p aegis-provider --test live_api -- --nocapture --test-threads 1
```

## Kết quả

| Giả định của provider | Kết quả | Số liệu |
| --- | --- | --- |
| Lượt 1 ghi cache, lượt 2 cùng prefix đọc cache | **Đúng** | Haiku 5.5, system ~5.300 token: lượt 1 ghi 5.271, đọc 0. Lượt 2 đọc 5.264, ghi 7 (phần đuôi mới, ở breakpoint tự động cấp request) |
| Breakpoint tự động cấp request (`cache_control` ở gốc body, BP4) | **Được nhận** | 7 token đuôi được ghi ở lượt 2 |
| Cache 1 giờ (`{"type": "ephemeral", "ttl": "1h"}`) | **Đúng** | System 1h: `ephemeral_1h_input_tokens` 5.264, BP4 vẫn 5 phút: `ephemeral_5m_input_tokens` 7 |
| `usage.cache_creation` tách theo TTL (ledger dựa vào để tính giá ghi) | **Có**, với Claude API | Như dòng trên. Bedrock: chưa kiểm, xem #55 |
| Tham số effort `output_config.effort` | **Đúng** | `low` và `medium` với Sonnet 5.5, `high` với Opus 5.5 đều được nhận |
| | | Giá trị sai bị từ chối: `output_config.effort: Input should be 'low', 'medium', 'high', 'xhigh' or 'max'`, tức API có đọc trường này |
| | | `effort` đặt ở gốc body bị từ chối: `Extra inputs are not permitted` |
| Ngưỡng tối thiểu để cache 512 token (Proposal §4) | **Khớp** | Haiku 5.5: prompt 415 token không được cache; prompt ~610 token được ghi 599 token |

Chi phí cả lượt đo: dưới $0,05.

## Hệ quả

- Không phải sửa code: hình dạng request của provider và cách ledger tách giá ghi cache theo TTL đều đúng với Claude API.
- API có thêm mức effort `xhigh` và `max` mà `Effort` chưa có. Hiện không cấu hình nào dùng; thêm khi cần.
- Phần Bedrock của #6 (profile `jp.`, ngưỡng token tối thiểu, `cache_creation` có tách theo TTL hay không) cần quyền AWS, nên để ở #55.
