# aegis-code

PoC Agentic Code Maintenance Platform: nhận một issue bảo trì (bug, lỗi CI, cảnh báo Sonar) và trả về một PR đã qua kiểm chứng, chờ người duyệt.

Tài liệu thiết kế:

- [docs/design/00-proposal.md](docs/design/00-proposal.md) — Proposal (chuẩn khi có mâu thuẫn)
- [docs/design/01-architecture-detail.md](docs/design/01-architecture-detail.md) — Thiết kế kỹ thuật

## Cấu trúc thư mục

| Thư mục | Nội dung | Tham chiếu |
| --- | --- | --- |
| `src/orchestrator/` | Nhận issue, chạy vòng lặp, giữ trạng thái (dựa trên Agent SDK) | Thiết kế §2 |
| `src/router/` | Chọn model Light/Core/Expert, luật leo thang & dừng, trần $4 / 20 lượt | §5 |
| `src/aci/` | Công cụ `search_symbol`, `open`, `edit`, `run_tests`…; tích hợp Codebase-Memory MCP, rust-analyzer; thu gọn lịch sử | §3 |
| `src/gate_runner/` | fmt → clippy → test tái hiện → test toàn bộ → coverage → Sonar, trong Docker | §6.1 |
| `src/pr_writer/` | Mở PR theo mẫu, Golden Master "Test được phép đổi" | §6.2, §6.4 |
| `src/ledger/` | Ghi token, chi phí, kết quả cho từng issue (JSON) để tính KPI | §4.3, §8 |
| `src/sonar_batch/` | Chia nhóm cảnh báo Sonar, gộp nhánh integration | §3.4, §6.3 |
| `config/` | Cấu hình PoC (Claude API) và code khách (Bedrock `jp.`), Sonar Server/Cloud | §1, §5.1 |
| `prompts/` | System prompt, quy ước Rust, tóm tắt cụm module (BP2, sinh một lần) | §4.1 |
| `docker/` | Image sandbox (dựa trên image CI, `--network none`) | §7 |
| `policies/` | Allow-list cho `cargo deny` | §7 |
| `eval/issues/` | Bộ 30–50 issue đã đóng dùng để đo KPI | Proposal §8 |
| `scripts/` | Việc Phase 0: đo thời gian build/test, lập chỉ mục | Proposal §9 |
| `tests/` | Test của chính platform | — |

Dữ liệu sinh ra khi chạy (chỉ mục SQLite, log ledger, workspace) nằm trong `.workspace/` và `runs/`, không commit.
