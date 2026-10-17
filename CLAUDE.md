# aegis-code

Platform Rust (Cargo workspace, `crates/`) nhận issue bảo trì và trả về PR đã qua kiểm chứng. Tài liệu chuẩn: [docs/design/00-proposal.md](docs/design/00-proposal.md) (ưu tiên khi mâu thuẫn), [docs/design/01-architecture-detail.md](docs/design/01-architecture-detail.md), [docs/adr/](docs/adr/).

## Làm việc theo issue

Mọi việc đều đi qua một GitHub issue.

- **Nhánh:** `<số issue>-<mô-tả-ngắn>`, chữ thường, nối bằng gạch ngang. Ví dụ: `10-router-escalation`.
- **PR:** mô tả có dòng `Closes #<số>` để issue tự đóng khi merge. Một PR chỉ xử lý một issue.
- **Label:** mỗi issue có ít nhất một `area/*` theo crate hoặc mảng việc:
  - Crate: `area/provider`, `area/orchestrator`, `area/router`, `area/aci`, `area/gate`, `area/ledger`, `area/pr`, `area/sonar`
  - Mảng việc: `area/infra` (Docker, CI, policies, prompts), `area/eval` (bộ thử, KPI)
  - Platform (Phase 2): `area/server`, `area/connectors`, `area/web`, `area/security`, `area/deploy`
  - Thêm `research` cho việc cần đo/kiểm tra trước khi quyết, `tech-debt` cho việc còn nợ từ review.
- **Milestone:** `Phase 0 – Chuẩn bị`, `Phase 1 – MVP / PoC`, `Phase 2 – Platform` hoặc `Phase 3 – Mở rộng` theo lộ trình ở Proposal §9 và [ADR 0002](docs/adr/0002-full-platform-mvp-as-checkpoint.md). Việc chưa xếp lịch thì để trống.
- **Phụ thuộc:** ghi ở cuối thân issue dạng `**Phụ thuộc:** #7, #9`. Không bắt đầu issue khi phụ thuộc chưa đóng.
- Việc mới phát sinh trong lúc làm → tạo issue mới, không gộp vào PR đang mở.

## Trước khi mở PR

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p aegis-provider --all-targets --features bedrock -- -D warnings
cargo test --workspace
cargo test -p aegis-provider --features bedrock
```

Commit theo Conventional Commits (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `chore:`, `ci:`).
