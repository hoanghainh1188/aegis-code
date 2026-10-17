# aegis-code

Agentic Code Maintenance Platform: nhận một issue bảo trì (bug, lỗi CI, cảnh báo Sonar) và trả về một PR đã qua kiểm chứng, chờ người duyệt.

Tài liệu thiết kế:

- [docs/design/00-proposal.md](docs/design/00-proposal.md) — Proposal (chuẩn khi có mâu thuẫn)
- [docs/design/01-architecture-detail.md](docs/design/01-architecture-detail.md) — Thiết kế kỹ thuật
- [docs/adr/](docs/adr/) — Các quyết định kiến trúc (ADR)

Platform viết bằng Rust (Cargo workspace), tự làm vòng lặp agent — xem [ADR 0001](docs/adr/0001-platform-in-rust.md).

```bash
cargo build
cargo test --workspace
cargo test -p aegis-provider --features bedrock   # kèm backend Bedrock
cargo run -p aegis-cli
```

## Cấu trúc thư mục

| Thư mục | Nội dung | Tham chiếu |
| --- | --- | --- |
| `crates/aegis-cli/` | Binary `aegis`: chạy một issue hoặc một batch | — |
| `crates/aegis-orchestrator/` | Vòng lặp agent cho một issue, giữ trạng thái, thu gọn lịch sử | Thiết kế §2, §3.3 |
| `crates/aegis-provider/` | Trait `ModelProvider`: Claude API, Bedrock; đặt `cache_control`; chặn model không có `jp.` | §4, §5.1, §7 |
| `crates/aegis-router/` | Chọn model Light/Core/Expert, luật leo thang & dừng, trần $4 / 20 lượt | §5 |
| `crates/aegis-aci/` | Công cụ `search_symbol`, `open`, `edit`, `run_tests`… phục vụ qua MCP | §3.1 |
| `crates/aegis-gate/` | fmt → clippy → test tái hiện → test toàn bộ → coverage → Sonar, trong Docker | §6.1 |
| `crates/aegis-pr/` | Mở PR theo mẫu, Golden Master "Test được phép đổi" | §6.2, §6.4 |
| `crates/aegis-ledger/` | Ghi token, chi phí, kết quả cho từng issue (JSON) để tính KPI | §4.3, §8 |
| `crates/aegis-sonar-batch/` | Chia nhóm cảnh báo Sonar, gộp nhánh integration | §3.4, §6.3 |
| `config/` | Cấu hình PoC (Claude API) và code khách (Bedrock `jp.`), Sonar Server/Cloud | §1, §5.1 |
| `prompts/` | System prompt, quy ước Rust, tóm tắt cụm module (BP2, sinh một lần) | §4.1 |
| `docker/` | Image sandbox (dựa trên image CI, `--network none`) | §7 |
| `policies/` | Allow-list cho `cargo deny` | §7 |
| `eval/issues/` | Bộ 30–50 issue đã đóng dùng để đo KPI | Proposal §8 |
| `scripts/` | Việc Phase 0: đo thời gian build/test, lập chỉ mục | Proposal §9 |
| `tests/` | Test tích hợp của platform | — |

Dữ liệu sinh ra khi chạy (chỉ mục SQLite, log ledger, workspace) nằm trong `.workspace/` và `runs/`, không commit.
