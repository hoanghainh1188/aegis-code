# Bedrock `cache_creation` tách theo TTL (#55)

Ngày: 2026-11-09. Mục tiêu của #55: biết Bedrock `InvokeModel` (Anthropic, `ap-northeast-1`, profile `jp.`) có trả `usage.cache_creation.ephemeral_5m/1h_input_tokens` hay không, và đảm bảo ledger không tính dư ~60% khi ghi cache 5 phút bị gộp vào giá 1 giờ.

## Kết luận

| Giả định | Kết quả | Nguồn |
| --- | --- | --- |
| Bedrock `InvokeModel` trả `usage.cache_creation` tách theo TTL | **Có** (cùng shape với Claude API) | Log model-invocation thật trong [LiteLLM #29432](https://github.com/BerriAI/litellm/issues/29432): `cache_creation: { ephemeral_5m_input_tokens, ephemeral_1h_input_tokens }` cạnh `cache_creation_input_tokens` |
| Claude API trả tách TTL | **Có** (đã đo) | [2026-10-22-claude-api-live.md](2026-10-22-claude-api-live.md) |
| Gọi live `jp.` trên máy này | **Chưa** — không có AWS credentials | `aws sts get-caller-identity` → NoCredentials |

## Hệ quả cho code

1. Deserialiser `Usage` / `CacheCreation` sẵn có vẫn đúng khi Bedrock trả tách.
2. Fallback (#55): sau mỗi response, provider gọi `Usage::attribute_writes_from_plan(request.cache)`. Chỉ chạy khi backend ghi cache nhưng **không** tách TTL — gán toàn bộ write theo `CachePlan.static_ttl`. Ledger `TokenCounts::from_usage` không đổi công thức; phần còn lại chưa gán vẫn tính giá 1h (an toàn cho ngân sách).
3. Live test gated: `crates/aegis-provider/tests/live_bedrock.rs`.

```bash
AEGIS_LIVE_BEDROCK=1 cargo test -p aegis-provider --features bedrock \
  --test live_bedrock -- --nocapture --test-threads 1
```

Khi có quyền AWS Tokyo, chạy lệnh trên và cập nhật bảng kết luận bằng số liệu máy mình (thay hàng “Chưa”).
