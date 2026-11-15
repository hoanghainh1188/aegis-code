# Golden Master & Ephemeral Sidecar Benchmark Suite (#454)

Controlled benchmark suite evaluating:
1. **Golden Master Characterization Testing ([Design §6.2](../../docs/design/01-architecture-detail.md#62-golden-master-characterization-testing)):** regression detection rate on untested legacy repositories (Python and Rust) compared against a repro-only baseline, plus enforcement of `allowed_test_changes` / `update_references` for intentional behavior modifications.
2. **Ephemeral Database Sidecars ([ADR 0013](../../docs/adr/0013-ephemeral-database-sidecars.md), #490):** real `postgres:16-alpine` integration testing inside air-gapped Docker sidecar networks (`--internal`), measuring `--network none` failure vs sidecar success, per-step durations, and `SidecarReport` overhead (startup, per-command tmpfs resets, teardown).

## Cases ([`cases.toml`](cases.toml))

| Case | Repo | Sidecar | Scenario |
| --- | --- | --- | --- |
| `gm-01-shipping-threshold` | `legacy_python` | No | Silent VIP rounding regression caught by Golden Master |
| `gm-02-null-coupon` | `legacy_python` | No | Silent whitespace coupon regression caught by Golden Master |
| `gm-03-exempt-tax` | `legacy_python` | No | Silent CA surcharge regression caught by Golden Master |
| `gm-04-invoice-format` | `legacy_python` | No | Undeclared snapshot break blocked; passes when declared via `allowed_test_changes` |
| `gm-05-proration-credit` | `legacy_rust` | No | Silent renewal-credit regression caught by Golden Master |
| `gm-06-jpy-rounding` | `legacy_rust` | No | Silent USD minor-unit truncation regression caught by Golden Master |
| `gm-07-idempotent-payment` | `legacy_postgres_python` | `postgres:16-alpine` | Fails `--network none`; silent DB overwrite slips past repro-only gate, caught by Golden Master |
| `gm-08-refund-balance` | `legacy_postgres_python` | `postgres:16-alpine` | Full gate pass + coverage + sidecar freshness across resets (`SidecarReport`) |

## Execution

Requires Docker daemon with `aegis-sandbox-python:latest` and `postgres:16-alpine` loaded (Docker cases skip cleanly otherwise unless `AEGIS_REQUIRE_DOCKER=1`), plus Rust `cargo` on `$PATH`:

```bash
cargo test -p aegis-gate --test golden_master_sidecar_eval -- --nocapture
```

Measured findings are recorded in [docs/measurements/2026-11-15-sidecar-golden-master.md](../../docs/measurements/2026-11-15-sidecar-golden-master.md).
