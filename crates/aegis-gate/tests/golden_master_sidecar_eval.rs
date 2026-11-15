//! Empirical benchmark suite for #454: Golden Master characterization testing
//! (Design §6.2) and ephemeral PostgreSQL sidecars (ADR 0013, #490).
#![cfg(unix)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use aegis_gate::{
    AuditPolicy, CancelToken, GateReport, GateRequest, Hardening, Limits, ReferenceGroup, Repro,
    Sandbox, SidecarSpec, Sidecars, StepName, StepStatus, run_gate,
};
use aegis_lang::Language;
use common::{git, profile, tools_available};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CasesFile {
    case: Vec<CaseMeta>,
}

#[derive(Debug, Deserialize)]
struct CaseMeta {
    id: String,
    repo_kind: String,
    language: String,
    requires_sidecar: bool,
    scenario: String,
}

fn manifest_cases() -> CasesFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/golden-master/cases.toml");
    let raw = std::fs::read_to_string(&path).expect("read eval/golden-master/cases.toml");
    toml::from_str(&raw).expect("parse eval/golden-master/cases.toml")
}

/// Checks whether Docker daemon is available and all required images exist locally.
fn docker_ready(images: &[&str]) -> bool {
    let ok = images.iter().all(|image| {
        Command::new("docker")
            .args(["image", "inspect", image])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if !ok {
        assert!(
            std::env::var("AEGIS_REQUIRE_DOCKER").as_deref() != Ok("1"),
            "docker or one of {images:?} is missing"
        );
        eprintln!("skipping: docker or one of {images:?} is missing");
    }
    ok
}

/// Temporary git repository rooted in `std::env::temp_dir()`, cleaned on drop.
struct TempRepo {
    root: PathBuf,
}

impl TempRepo {
    fn create(prefix: &str, files: &[(&str, &str)]) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "aegis-gm-{prefix}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let repo = Self { root };
        repo.write(
            ".gitignore",
            "target/\n__pycache__/\n.pytest_cache/\n.coverage\ncoverage.xml\nlcov.info\n",
        );
        for (path, body) in files {
            repo.write(path, body);
        }
        git(&repo.dir(), &["init", "-q", "-b", "main"]);
        git(&repo.dir(), &["add", "-A"]);
        git(
            &repo.dir(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                "base",
            ],
        );
        repo
    }

    fn dir(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.dir().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir().join(rel)).unwrap()
    }

    fn replace(&self, rel: &str, from: &str, to: &str) {
        let text = self.read(rel);
        assert!(text.contains(from), "`{from}` not found in {rel}");
        self.write(rel, &text.replacen(from, to, 1));
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn python_sandbox(profile: &aegis_lang::ResolvedProfile) -> Sandbox {
    Sandbox {
        docker: "docker".into(),
        image: profile.image.clone(),
        mounts: vec![],
        env: vec![],
        network: None,
        read_only_mounts: true,
        hardening: Hardening::new(Limits::default(), &profile.profile.sandbox),
    }
}

fn postgres_sidecar() -> SidecarSpec {
    SidecarSpec {
        name: "postgres".into(),
        image: "postgres:16-alpine".into(),
        user: "70:70".into(),
        env: BTreeMap::from([
            ("POSTGRES_DB".into(), "orders".into()),
            ("POSTGRES_HOST_AUTH_METHOD".into(), "trust".into()),
        ]),
        tmpfs: vec![
            "/var/lib/postgresql/data".into(),
            "/var/run/postgresql".into(),
        ],
        ready: vec![
            "pg_isready".into(),
            "-h".into(),
            "localhost".into(),
            "-U".into(),
            "postgres".into(),
        ],
        ready_timeout_s: 30,
        localhost: false,
        test_env: BTreeMap::from([
            ("PGHOST".into(), "postgres".into()),
            ("PGPORT".into(), "5432".into()),
            ("PGUSER".into(), "postgres".into()),
            ("PGDATABASE".into(), "orders".into()),
        ]),
        memory_mb: Some(512),
        cpus: Some(1),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_gate(
    repo: &TempRepo,
    prof: &aegis_lang::ResolvedProfile,
    repro: Option<&Repro>,
    sandbox: Option<Sandbox>,
    allowed_tests: &[&str],
    reference_groups: &[ReferenceGroup],
    sidecars: Option<Sidecars>,
) -> (GateReport, Duration) {
    let started = Instant::now();
    let allowed = BTreeSet::new();
    let (dir, work) = (repo.dir(), repo.work());
    let report = run_gate(&GateRequest {
        repo: &dir,
        base: "main",
        profile: prof,
        allowed: &allowed,
        repro,
        work_dir: &work,
        env: vec![],
        step_timeout: Duration::from_secs(120),
        cancel: CancelToken::new(),
        sandbox,
        fetch: None,
        build_cache: None,
        allowed_tests: allowed_tests.iter().map(|s| (*s).to_owned()).collect(),
        reference_groups: reference_groups.to_vec(),
        sonar: None,
        audit: AuditPolicy::default(),
        scope: aegis_gate::ScopePolicy::default(),
        secrets: aegis_gate::SecretPolicy::default(),
        parts: vec![],
        sidecars,
        dependency_policy: Default::default(),
    })
    .unwrap();
    (report, started.elapsed())
}

const LEGACY_BILLING_PYPROJECT: &str = r#"[project]
name = "legacy-billing"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = []

[tool.pytest.ini_options]
pythonpath = ["."]
testpaths = ["tests"]

[tool.coverage.run]
source = ["billing"]
"#;

const LEGACY_BILLING_PRICING_PY: &str = r#"from decimal import Decimal, ROUND_HALF_UP


def calculate_shipping(subtotal_cents: int) -> int:
    # Bug GM-01: boundary uses > instead of >= 10000 cents ($100.00).
    if subtotal_cents > 10_000:
        return 0
    return 795


def apply_discount(subtotal_cents: int, tier: str, coupon: str) -> int:
    # Bug GM-02: fails with AttributeError when coupon is None.
    code = coupon.strip().upper()
    if code == "SAVE20":
        rate = Decimal("0.20")
    elif code == "NONE" or code == "":
        rate = Decimal("0.125") if tier == "vip" else Decimal("0.00")
    else:
        rate = Decimal("0.05")
    discount = (Decimal(subtotal_cents) * rate).quantize(
        Decimal("1"), rounding=ROUND_HALF_UP
    )
    return subtotal_cents - int(discount)


def compute_tax(subtotal_cents: int, category: str, state: str) -> int:
    # Bug GM-03: missing OR books tax exemption.
    if state == "DE":
        return 0
    if state == "CA":
        base = Decimal(subtotal_cents) * Decimal("0.08")
        surcharge = (
            Decimal(subtotal_cents) * Decimal("0.02")
            if category == "electronics"
            else Decimal("0")
        )
        return int((base + surcharge).quantize(Decimal("1"), rounding=ROUND_HALF_UP))
    return int(
        (Decimal(subtotal_cents) * Decimal("0.05")).quantize(
            Decimal("1"), rounding=ROUND_HALF_UP
        )
    )


def format_invoice(order_id: str, total_cents: int) -> str:
    # Candidate GM-04 intentional output format change target.
    return f"INV:{order_id}:{total_cents}"
"#;

const GOLDEN_MASTER_BILLING_TEST: &str = r#"from billing.pricing import (
    apply_discount,
    calculate_shipping,
    compute_tax,
)


def test_gm_vip_rounding_preserved():
    assert apply_discount(1005, "vip", "") == 879
    assert apply_discount(2500, "vip", "") == 2187


def test_gm_whitespace_coupon_uses_tier_fallback():
    assert apply_discount(1000, "vip", "   ") == 875
    assert apply_discount(1000, "standard", "   ") == 1000


def test_gm_ca_electronics_surcharge_preserved():
    assert compute_tax(10_000, "electronics", "CA") == 1000
    assert compute_tax(10_000, "books", "CA") == 800
    assert calculate_shipping(5000) == 795
"#;

struct LegacyPyCase {
    id: &'static str,
    repro_name: &'static str,
    repro_code: &'static str,
    old_snippet: &'static str,
    mutant_snippet: &'static str,
    clean_snippet: &'static str,
}

const PYTHON_CASES: [LegacyPyCase; 3] = [
    LegacyPyCase {
        id: "gm-01-shipping-threshold",
        repro_name: "tests/test_repro.py::test_free_shipping_at_10000",
        repro_code: r#"from billing.pricing import calculate_shipping


def test_free_shipping_at_10000():
    assert calculate_shipping(10_000) == 0
"#,
        old_snippet: r#"def calculate_shipping(subtotal_cents: int) -> int:
    # Bug GM-01: boundary uses > instead of >= 10000 cents ($100.00).
    if subtotal_cents > 10_000:
        return 0
    return 795


def apply_discount(subtotal_cents: int, tier: str, coupon: str) -> int:
    # Bug GM-02: fails with AttributeError when coupon is None.
    code = coupon.strip().upper()
    if code == "SAVE20":
        rate = Decimal("0.20")
    elif code == "NONE" or code == "":
        rate = Decimal("0.125") if tier == "vip" else Decimal("0.00")
    else:
        rate = Decimal("0.05")
    discount = (Decimal(subtotal_cents) * rate).quantize(
        Decimal("1"), rounding=ROUND_HALF_UP
    )
    return subtotal_cents - int(discount)"#,
        mutant_snippet: r#"def calculate_shipping(subtotal_cents: int) -> int:
    if subtotal_cents >= 10_000:
        return 0
    return 795


def apply_discount(subtotal_cents: int, tier: str, coupon: str) -> int:
    code = coupon.strip().upper()
    if code == "SAVE20":
        rate = Decimal("0.20")
    elif code == "NONE" or code == "":
        rate = Decimal("0.125") if tier == "vip" else Decimal("0.00")
    else:
        rate = Decimal("0.05")
    # Silent regression: truncates VIP rounding instead of ROUND_HALF_UP.
    discount = int(Decimal(subtotal_cents) * rate)
    return subtotal_cents - discount"#,
        clean_snippet: r#"def calculate_shipping(subtotal_cents: int) -> int:
    if subtotal_cents >= 10_000:
        return 0
    return 795


def apply_discount(subtotal_cents: int, tier: str, coupon: str) -> int:
    code = coupon.strip().upper()
    if code == "SAVE20":
        rate = Decimal("0.20")
    elif code == "NONE" or code == "":
        rate = Decimal("0.125") if tier == "vip" else Decimal("0.00")
    else:
        rate = Decimal("0.05")
    discount = (Decimal(subtotal_cents) * rate).quantize(
        Decimal("1"), rounding=ROUND_HALF_UP
    )
    return subtotal_cents - int(discount)"#,
    },
    LegacyPyCase {
        id: "gm-02-null-coupon",
        repro_name: "tests/test_repro.py::test_none_coupon_preserves_tier",
        repro_code: r#"from billing.pricing import apply_discount


def test_none_coupon_preserves_tier():
    assert apply_discount(1000, "vip", None) == 875
"#,
        old_snippet: r#"def apply_discount(subtotal_cents: int, tier: str, coupon: str) -> int:
    # Bug GM-02: fails with AttributeError when coupon is None.
    code = coupon.strip().upper()
    if code == "SAVE20":
        rate = Decimal("0.20")
    elif code == "NONE" or code == "":
        rate = Decimal("0.125") if tier == "vip" else Decimal("0.00")
    else:
        rate = Decimal("0.05")"#,
        mutant_snippet: r#"def apply_discount(subtotal_cents: int, tier: str, coupon: str | None) -> int:
    # Silent regression: skips .strip(), so whitespace falls into default branch.
    code = coupon.upper() if coupon is not None else ""
    if code == "SAVE20":
        rate = Decimal("0.20")
    elif code == "NONE" or code == "":
        rate = Decimal("0.125") if tier == "vip" else Decimal("0.00")
    else:
        rate = Decimal("0.05")"#,
        clean_snippet: r#"def apply_discount(subtotal_cents: int, tier: str, coupon: str | None) -> int:
    code = (coupon or "").strip().upper()
    if code == "SAVE20":
        rate = Decimal("0.20")
    elif code == "NONE" or code == "":
        rate = Decimal("0.125") if tier == "vip" else Decimal("0.00")
    else:
        rate = Decimal("0.05")"#,
    },
    LegacyPyCase {
        id: "gm-03-exempt-tax",
        repro_name: "tests/test_repro.py::test_books_tax_exempt_in_or",
        repro_code: r#"from billing.pricing import compute_tax


def test_books_tax_exempt_in_or():
    assert compute_tax(10_000, "books", "OR") == 0
"#,
        old_snippet: r#"def compute_tax(subtotal_cents: int, category: str, state: str) -> int:
    # Bug GM-03: missing OR books tax exemption.
    if state == "DE":
        return 0
    if state == "CA":
        base = Decimal(subtotal_cents) * Decimal("0.08")
        surcharge = (
            Decimal(subtotal_cents) * Decimal("0.02")
            if category == "electronics"
            else Decimal("0")
        )
        return int((base + surcharge).quantize(Decimal("1"), rounding=ROUND_HALF_UP))"#,
        mutant_snippet: r#"def compute_tax(subtotal_cents: int, category: str, state: str) -> int:
    if state == "DE" or (state == "OR" and category == "books"):
        return 0
    if state == "CA":
        base = Decimal(subtotal_cents) * Decimal("0.08")
        # Silent regression: drops CA electronics 2% surcharge.
        return int(base.quantize(Decimal("1"), rounding=ROUND_HALF_UP))"#,
        clean_snippet: r#"def compute_tax(subtotal_cents: int, category: str, state: str) -> int:
    if state == "DE" or (state == "OR" and category == "books"):
        return 0
    if state == "CA":
        base = Decimal(subtotal_cents) * Decimal("0.08")
        surcharge = (
            Decimal(subtotal_cents) * Decimal("0.02")
            if category == "electronics"
            else Decimal("0")
        )
        return int((base + surcharge).quantize(Decimal("1"), rounding=ROUND_HALF_UP))"#,
    },
];

fn billing_base_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("pyproject.toml", LEGACY_BILLING_PYPROJECT),
        ("billing/__init__.py", ""),
        ("billing/pricing.py", LEGACY_BILLING_PRICING_PY),
    ]
}

#[test]
fn golden_master_manifest_matches_benchmark_cases() {
    let manifest = manifest_cases();
    assert_eq!(manifest.case.len(), 8);
    let ids: Vec<&str> = manifest.case.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "gm-01-shipping-threshold",
            "gm-02-null-coupon",
            "gm-03-exempt-tax",
            "gm-04-invoice-format",
            "gm-05-proration-credit",
            "gm-06-jpy-rounding",
            "gm-07-idempotent-payment",
            "gm-08-refund-balance",
        ]
    );
    assert!(manifest.case.iter().any(|c| c.requires_sidecar));
    assert!(manifest.case.iter().any(|c| c.repo_kind == "legacy_rust"));
    assert!(
        manifest
            .case
            .iter()
            .any(|c| c.scenario == "undeclared_change" && c.language == "python")
    );
}

#[test]
fn legacy_python_golden_master_catches_silent_regressions() {
    if !docker_ready(&["aegis-sandbox-python:latest"]) {
        return;
    }
    let prof = profile(Language::Python, "");
    let mut baseline_escaped = 0usize;
    let mut gm_caught = 0usize;
    let mut clean_passed = 0usize;

    for case in &PYTHON_CASES {
        // 1) Baseline (no Golden Master): only the bug repro test exists.
        let base_repo = TempRepo::create(case.id, &billing_base_files());
        base_repo.write("tests/test_repro.py", case.repro_code);
        base_repo.replace("billing/pricing.py", case.old_snippet, case.mutant_snippet);
        let repro = Repro {
            test: case.repro_name.into(),
            files: vec!["tests/test_repro.py".into()],
        };
        let (baseline_report, baseline_dur) = execute_gate(
            &base_repo,
            &prof,
            Some(&repro),
            Some(python_sandbox(&prof)),
            &[],
            &[],
            None,
        );
        assert!(
            baseline_report.passed(),
            "{} baseline unexpectedly failed:\n{baseline_report}",
            case.id
        );
        baseline_escaped += 1;

        // 2) Verify Golden Master passes on unmodified base (Design §6.2 Step 1),
        // then apply the same mutated patch -> must fail at StepName::Test.
        let mut gm_files = billing_base_files();
        gm_files.push(("tests/test_golden_master.py", GOLDEN_MASTER_BILLING_TEST));
        let gm_repo = TempRepo::create(&format!("{}-gm", case.id), &gm_files);
        let gm_base_check = Command::new("docker")
            .args([
                "run",
                "--rm",
                "--network",
                "none",
                "--workdir",
                "/work",
                "--volume",
                &format!("{}:/work:ro", gm_repo.dir().display()),
                &prof.image,
                "python",
                "-m",
                "pytest",
                "tests/test_golden_master.py",
            ])
            .output()
            .unwrap();
        assert!(
            gm_base_check.status.success(),
            "GM suite must pass on base for {}: {}",
            case.id,
            String::from_utf8_lossy(&gm_base_check.stdout)
        );

        gm_repo.write("tests/test_repro.py", case.repro_code);
        gm_repo.replace("billing/pricing.py", case.old_snippet, case.mutant_snippet);
        let (gm_mutant_report, gm_mutant_dur) = execute_gate(
            &gm_repo,
            &prof,
            Some(&repro),
            Some(python_sandbox(&prof)),
            &[],
            &[],
            None,
        );
        let test_step = gm_mutant_report.step(StepName::Test).unwrap();
        assert_eq!(
            test_step.status,
            StepStatus::Failed,
            "{} GM should catch silent regression:\n{gm_mutant_report}",
            case.id
        );
        gm_caught += 1;

        // 3) Clean fix with Golden Master active -> 0 false positives.
        let clean_repo = TempRepo::create(&format!("{}-clean", case.id), &gm_files);
        clean_repo.write("tests/test_repro.py", case.repro_code);
        clean_repo.replace("billing/pricing.py", case.old_snippet, case.clean_snippet);
        let (clean_report, clean_dur) = execute_gate(
            &clean_repo,
            &prof,
            Some(&repro),
            Some(python_sandbox(&prof)),
            &[],
            &[],
            None,
        );
        assert!(
            clean_report.passed(),
            "{} clean fix with GM failed:\n{clean_report}",
            case.id
        );
        clean_passed += 1;

        eprintln!(
            "[{}] baseline_ms={} (passed={}), gm_mutant_ms={} (caught={}), gm_clean_ms={} (passed={})",
            case.id,
            baseline_dur.as_millis(),
            baseline_report.passed(),
            gm_mutant_dur.as_millis(),
            !gm_mutant_report.passed(),
            clean_dur.as_millis(),
            clean_report.passed(),
        );
    }

    assert_eq!(baseline_escaped, 3);
    assert_eq!(gm_caught, 3);
    assert_eq!(clean_passed, 3);
}

#[test]
fn legacy_python_snapshot_change_requires_allowed_declaration() {
    if !docker_ready(&["aegis-sandbox-python:latest"]) {
        return;
    }
    let prof = profile(
        Language::Python,
        r#"
[gate]
update_references = ["python", "-m", "billing.update_ref", "{test}"]
reference_dirs = ["tests/ref"]
"#,
    );
    let update_ref_py = r#"from pathlib import Path
import sys

from billing.pricing import format_invoice


def main() -> None:
    _ = sys.argv[1:]
    out = Path("tests/ref/invoice.txt")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(format_invoice("42", 1995) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
"#;
    let snapshot_test = r#"from pathlib import Path

from billing.pricing import format_invoice


def test_invoice_snapshot():
    expected = Path("tests/ref/invoice.txt").read_text(encoding="utf-8").strip()
    assert format_invoice("42", 1995) == expected
"#;
    let mut files = billing_base_files();
    files.push(("billing/update_ref.py", update_ref_py));
    files.push(("tests/test_snapshot.py", snapshot_test));
    files.push(("tests/ref/invoice.txt", "INV:42:1995\n"));

    let undeclared = TempRepo::create("gm-04-undeclared", &files);
    undeclared.replace(
        "billing/pricing.py",
        r#"return f"INV:{order_id}:{total_cents}""#,
        r#"return f"INVOICE#{order_id}={total_cents}""#,
    );
    let (blocked_report, blocked_dur) = execute_gate(
        &undeclared,
        &prof,
        None,
        Some(python_sandbox(&prof)),
        &[],
        &[],
        None,
    );
    let test_step = blocked_report.step(StepName::Test).unwrap();
    assert_eq!(test_step.status, StepStatus::Failed, "{blocked_report}");
    assert!(
        test_step.detail.contains("allowed_test_changes"),
        "{}",
        test_step.detail
    );

    let declared = TempRepo::create("gm-04-declared", &files);
    declared.replace(
        "billing/pricing.py",
        r#"return f"INV:{order_id}:{total_cents}""#,
        r#"return f"INVOICE#{order_id}={total_cents}""#,
    );
    let (allowed_report, allowed_dur) = execute_gate(
        &declared,
        &prof,
        None,
        Some(python_sandbox(&prof)),
        &["test_invoice_snapshot"],
        &[],
        None,
    );
    assert!(allowed_report.passed(), "{allowed_report}");
    assert_eq!(allowed_report.references.tests, ["test_invoice_snapshot"]);
    assert_eq!(allowed_report.references.files, ["tests/ref/invoice.txt"]);
    assert_eq!(declared.read("tests/ref/invoice.txt"), "INVOICE#42=1995\n");

    eprintln!(
        "[gm-04-invoice-format] undeclared_blocked_ms={} declared_passed_ms={}",
        blocked_dur.as_millis(),
        allowed_dur.as_millis()
    );
}

const RUST_CARGO_TOML: &str = r#"[package]
name = "legacy_ledger"
version = "0.1.0"
edition = "2021"
"#;

const RUST_CARGO_LOCK: &str = r#"# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "legacy_ledger"
version = "0.1.0"
"#;

const RUST_LEGACY_LIB: &str = r#"/// Partial-month subscription upgrade proration in cents.
pub fn proration_credit(monthly_cents: i64, days_used: i64, days_in_month: i64) -> i64 {
    // Bug GM-05: subtracts 1 day from elapsed window, under-crediting upgrades.
    let elapsed = (days_used - 1).max(0);
    let remaining = (days_in_month - elapsed).max(0);
    (monthly_cents * remaining) / days_in_month
}

/// Converts major currency amount (milles) into currency minor units.
pub fn to_minor_units(millis: i64, currency: &str) -> i64 {
    // Bug GM-06: treats JPY identically to 2-decimal currencies.
    let _ = currency;
    millis / 10
}
"#;

const RUST_GM_TEST: &str = r#"use legacy_ledger::{proration_credit, to_minor_units};

#[test]
fn gm_full_month_renewal_credit() {
    assert_eq!(proration_credit(3000, 0, 30), 3000);
    assert_eq!(proration_credit(1500, 0, 30), 1500);
}

#[test]
fn gm_usd_and_eur_minor_units() {
    assert_eq!(to_minor_units(12340, "USD"), 1234);
    assert_eq!(to_minor_units(9990, "EUR"), 999);
}
"#;

#[test]
fn legacy_rust_golden_master_catches_silent_regressions() {
    if !tools_available(&[&["cargo", "--version"]]) {
        return;
    }
    let prof = profile(
        Language::Rust,
        r#"
[gate]
coverage = false
"#,
    );

    struct RustCase {
        id: &'static str,
        repro_name: &'static str,
        repro_file: &'static str,
        old_fn: &'static str,
        mutant_fn: &'static str,
        clean_fn: &'static str,
    }
    let cases = [
        RustCase {
            id: "gm-05-proration-credit",
            repro_name: "repro_partial_month_proration",
            repro_file: r#"use legacy_ledger::proration_credit;

#[test]
fn repro_partial_month_proration() {
    assert_eq!(proration_credit(3000, 10, 30), 2000);
}
"#,
            old_fn: r#"pub fn proration_credit(monthly_cents: i64, days_used: i64, days_in_month: i64) -> i64 {
    // Bug GM-05: subtracts 1 day from elapsed window, under-crediting upgrades.
    let elapsed = (days_used - 1).max(0);
    let remaining = (days_in_month - elapsed).max(0);
    (monthly_cents * remaining) / days_in_month
}"#,
            mutant_fn: r#"pub fn proration_credit(monthly_cents: i64, days_used: i64, days_in_month: i64) -> i64 {
    // Silent regression: caps credit at days_in_month - 1, breaking zero-usage renewals.
    let remaining = (days_in_month - days_used).clamp(0, days_in_month - 1);
    (monthly_cents * remaining) / days_in_month
}"#,
            clean_fn: r#"pub fn proration_credit(monthly_cents: i64, days_used: i64, days_in_month: i64) -> i64 {
    let remaining = (days_in_month - days_used).clamp(0, days_in_month);
    (monthly_cents * remaining) / days_in_month
}"#,
        },
        RustCase {
            id: "gm-06-jpy-rounding",
            repro_name: "repro_jpy_zero_decimals",
            repro_file: r#"use legacy_ledger::to_minor_units;

#[test]
fn repro_jpy_zero_decimals() {
    assert_eq!(to_minor_units(150_000, "JPY"), 150);
}
"#,
            old_fn: r#"pub fn to_minor_units(millis: i64, currency: &str) -> i64 {
    // Bug GM-06: treats JPY identically to 2-decimal currencies.
    let _ = currency;
    millis / 10
}"#,
            mutant_fn: r#"pub fn to_minor_units(millis: i64, currency: &str) -> i64 {
    // Silent regression: divides USD/EUR by 1000 instead of 10, truncating cents.
    if currency == "JPY" {
        millis / 1000
    } else {
        (millis / 1000) * 100
    }
}"#,
            clean_fn: r#"pub fn to_minor_units(millis: i64, currency: &str) -> i64 {
    if currency == "JPY" {
        millis / 1000
    } else {
        millis / 10
    }
}"#,
        },
    ];

    for case in cases {
        let base_files = vec![
            ("Cargo.toml", RUST_CARGO_TOML),
            ("Cargo.lock", RUST_CARGO_LOCK),
            ("src/lib.rs", RUST_LEGACY_LIB),
        ];
        let repro = Repro {
            test: case.repro_name.into(),
            files: vec!["tests/repro.rs".into()],
        };

        let base_repo = TempRepo::create(case.id, &base_files);
        base_repo.write("tests/repro.rs", case.repro_file);
        base_repo.replace("src/lib.rs", case.old_fn, case.mutant_fn);
        let (baseline_report, baseline_dur) =
            execute_gate(&base_repo, &prof, Some(&repro), None, &[], &[], None);
        assert!(
            baseline_report.passed(),
            "{} baseline unexpectedly failed:\n{baseline_report}",
            case.id
        );

        let mut gm_files = base_files;
        gm_files.push(("tests/golden_master.rs", RUST_GM_TEST));
        let gm_repo = TempRepo::create(&format!("{}-gm", case.id), &gm_files);
        let base_ok = Command::new("cargo")
            .args(["test", "--test", "golden_master"])
            .current_dir(gm_repo.dir())
            .output()
            .unwrap();
        assert!(base_ok.status.success(), "GM must pass on base");
        gm_repo.write("tests/repro.rs", case.repro_file);
        gm_repo.replace("src/lib.rs", case.old_fn, case.mutant_fn);
        let (gm_mutant_report, gm_mutant_dur) =
            execute_gate(&gm_repo, &prof, Some(&repro), None, &[], &[], None);
        assert_eq!(
            gm_mutant_report.step(StepName::Test).unwrap().status,
            StepStatus::Failed,
            "{} GM should catch silent regression:\n{gm_mutant_report}",
            case.id
        );

        let clean_repo = TempRepo::create(&format!("{}-clean", case.id), &gm_files);
        clean_repo.write("tests/repro.rs", case.repro_file);
        clean_repo.replace("src/lib.rs", case.old_fn, case.clean_fn);
        let (clean_report, clean_dur) =
            execute_gate(&clean_repo, &prof, Some(&repro), None, &[], &[], None);
        assert!(
            clean_report.passed(),
            "{} clean fix with GM failed:\n{clean_report}",
            case.id
        );

        eprintln!(
            "[{}] baseline_ms={} (passed={}), gm_mutant_ms={} (caught={}), gm_clean_ms={} (passed={})",
            case.id,
            baseline_dur.as_millis(),
            baseline_report.passed(),
            gm_mutant_dur.as_millis(),
            !gm_mutant_report.passed(),
            clean_dur.as_millis(),
            clean_report.passed(),
        );
    }
}

const PGWIRE_PY: &str = r#"import os
import socket
import struct


def _read_exact(sock: socket.socket, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("unexpected EOF from postgres")
        buf.extend(chunk)
    return bytes(buf)


def _read_packet(sock: socket.socket) -> tuple[str, bytes]:
    tag = _read_exact(sock, 1).decode("ascii")
    length = struct.unpack("!I", _read_exact(sock, 4))[0]
    payload = _read_exact(sock, length - 4)
    return tag, payload


def query(sql: str) -> list[list[str | None]]:
    host = os.environ.get("PGHOST", "localhost")
    port = int(os.environ.get("PGPORT", "5432"))
    user = os.environ.get("PGUSER", "postgres")
    db = os.environ.get("PGDATABASE", "orders")
    rows: list[list[str | None]] = []
    err: str | None = None

    with socket.create_connection((host, port), timeout=5) as sock:
        params = (
            f"user\x00{user}\x00database\x00{db}\x00\x00".encode("utf-8")
        )
        startup = struct.pack("!I", 196608) + params
        sock.sendall(struct.pack("!I", len(startup) + 4) + startup)

        while True:
            tag, payload = _read_packet(sock)
            if tag == "E":
                raise RuntimeError(payload.decode("utf-8", errors="replace"))
            if tag == "Z":
                break

        q_body = sql.encode("utf-8") + b"\x00"
        sock.sendall(b"Q" + struct.pack("!I", len(q_body) + 4) + q_body)

        while True:
            tag, payload = _read_packet(sock)
            if tag == "D":
                ncols = struct.unpack("!H", payload[:2])[0]
                pos = 2
                row: list[str | None] = []
                for _ in range(ncols):
                    flen = struct.unpack("!i", payload[pos : pos + 4])[0]
                    pos += 4
                    if flen < 0:
                        row.append(None)
                    else:
                        row.append(payload[pos : pos + flen].decode("utf-8"))
                        pos += flen
                rows.append(row)
            elif tag == "E":
                err = payload.decode("utf-8", errors="replace")
            elif tag == "Z":
                break

        sock.sendall(b"X\x00\x00\x00\x04")

    if err is not None:
        raise RuntimeError(err)
    return rows
"#;

const ORDERS_REPO_PY: &str = r#"from orders.pgwire import query


def init_schema() -> None:
    query(
        """
        CREATE TABLE IF NOT EXISTS payment_orders (
            external_id TEXT PRIMARY KEY,
            amount_cents INTEGER NOT NULL,
            refunded_cents INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL
        );
        """
    )


def record_payment(external_id: str, amount_cents: int) -> None:
    # Bug GM-07: non-idempotent INSERT raises duplicate PK error on retries.
    query(
        f"INSERT INTO payment_orders (external_id, amount_cents, status) "
        f"VALUES ('{external_id}', {amount_cents}, 'paid');"
    )


def apply_refund(external_id: str, refund_cents: int) -> int:
    # Bug GM-08: does not clamp refund_cents against remaining balance.
    query(
        f"UPDATE payment_orders "
        f"SET refunded_cents = refunded_cents + {refund_cents} "
        f"WHERE external_id = '{external_id}';"
    )
    rows = query(
        f"SELECT amount_cents - refunded_cents FROM payment_orders "
        f"WHERE external_id = '{external_id}';"
    )
    return int(rows[0][0] or "0")


def get_order(external_id: str) -> tuple[int, int, str]:
    rows = query(
        f"SELECT amount_cents, refunded_cents, status FROM payment_orders "
        f"WHERE external_id = '{external_id}';"
    )
    amt, refunded, status = rows[0]
    return int(amt or "0"), int(refunded or "0"), status or ""
"#;

const ORDERS_PYPROJECT: &str = r#"[project]
name = "legacy-orders-db"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = []

[tool.pytest.ini_options]
pythonpath = ["."]
testpaths = ["tests"]

[tool.coverage.run]
source = ["orders"]
"#;

const ORDERS_REPRO_GM07: &str = r#"from orders.repo import get_order, init_schema, record_payment


def test_idempotent_payment_retry():
    init_schema()
    record_payment("ext-100", 5000)
    record_payment("ext-100", 5000)
    amt, refunded, status = get_order("ext-100")
    assert (amt, refunded, status) == (5000, 0, "paid")
"#;

const ORDERS_GM_TEST: &str = r#"from orders.repo import apply_refund, get_order, init_schema, record_payment


def test_gm_existing_order_amount_not_overwritten_and_refund_tracks_balance():
    init_schema()
    record_payment("ext-gm-1", 4200)
    # Replaying a duplicate webhook should keep original settled amount intact.
    try:
        record_payment("ext-gm-1", 9999)
    except RuntimeError:
        pass
    assert get_order("ext-gm-1") == (4200, 0, "paid")
    assert apply_refund("ext-gm-1", 1200) == 3000
    assert get_order("ext-gm-1")[1] == 1200
"#;

const ORDERS_REPRO_GM08: &str = r#"from orders.repo import apply_refund, get_order, init_schema, record_payment


def test_refund_clamped_to_remaining_balance():
    init_schema()
    record_payment("ext-refund-1", 2500)
    remaining = apply_refund("ext-refund-1", 4000)
    assert remaining == 0
    assert get_order("ext-refund-1")[1] == 2500
"#;

fn orders_base_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("pyproject.toml", ORDERS_PYPROJECT),
        ("orders/__init__.py", ""),
        ("orders/pgwire.py", PGWIRE_PY),
        ("orders/repo.py", ORDERS_REPO_PY),
    ]
}

#[test]
fn legacy_postgres_sidecar_and_golden_master_end_to_end() {
    if !docker_ready(&["aegis-sandbox-python:latest", "postgres:16-alpine"]) {
        return;
    }
    let prof = profile(Language::Python, "");
    let sidecar_cfg = Sidecars {
        docker: "docker".into(),
        specs: vec![postgres_sidecar()],
    };

    let mutant_record_payment = r#"def record_payment(external_id: str, amount_cents: int) -> None:
    # Silent regression: DO UPDATE overwrites pre-existing settled amount.
    query(
        f"INSERT INTO payment_orders (external_id, amount_cents, status) "
        f"VALUES ('{external_id}', {amount_cents}, 'paid') "
        f"ON CONFLICT (external_id) DO UPDATE "
        f"SET amount_cents = EXCLUDED.amount_cents;"
    )"#;
    let old_record_payment = r#"def record_payment(external_id: str, amount_cents: int) -> None:
    # Bug GM-07: non-idempotent INSERT raises duplicate PK error on retries.
    query(
        f"INSERT INTO payment_orders (external_id, amount_cents, status) "
        f"VALUES ('{external_id}', {amount_cents}, 'paid');"
    )"#;

    let repro_gm07 = Repro {
        test: "tests/test_repro_gm07.py::test_idempotent_payment_retry".into(),
        files: vec!["tests/test_repro_gm07.py".into()],
    };

    // 1) Under --network none (no sidecar), DB integration test fails cleanly.
    let no_net = TempRepo::create("gm-07-nonet", &orders_base_files());
    no_net.write("tests/test_repro_gm07.py", ORDERS_REPRO_GM07);
    no_net.replace("orders/repo.py", old_record_payment, mutant_record_payment);
    let (nonet_report, nonet_dur) = execute_gate(
        &no_net,
        &prof,
        Some(&repro_gm07),
        Some(python_sandbox(&prof)),
        &[],
        &[],
        None,
    );
    assert_eq!(
        nonet_report.step(StepName::ReproTest).unwrap().status,
        StepStatus::Failed,
        "--network none must fail DB test:\n{nonet_report}"
    );

    // 2) With postgres:16-alpine sidecar, repro-only baseline misses the silent UPSERT regression.
    let base_db = TempRepo::create("gm-07-baseline", &orders_base_files());
    base_db.write("tests/test_repro_gm07.py", ORDERS_REPRO_GM07);
    base_db.replace("orders/repo.py", old_record_payment, mutant_record_payment);
    let (db_baseline_report, db_baseline_dur) = execute_gate(
        &base_db,
        &prof,
        Some(&repro_gm07),
        Some(python_sandbox(&prof)),
        &[],
        &[],
        Some(sidecar_cfg.clone()),
    );
    assert!(
        db_baseline_report.passed(),
        "DB baseline without GM unexpectedly failed:\n{db_baseline_report}"
    );

    // 3) With postgres:16-alpine sidecar AND Golden Master DB characterization, regression is caught.
    let mut gm_db_files = orders_base_files();
    gm_db_files.push(("tests/test_gm_db.py", ORDERS_GM_TEST));
    let gm_db = TempRepo::create("gm-07-gm", &gm_db_files);
    gm_db.write("tests/test_repro_gm07.py", ORDERS_REPRO_GM07);
    gm_db.replace("orders/repo.py", old_record_payment, mutant_record_payment);
    let (db_mutant_report, db_mutant_dur) = execute_gate(
        &gm_db,
        &prof,
        Some(&repro_gm07),
        Some(python_sandbox(&prof)),
        &[],
        &[],
        Some(sidecar_cfg.clone()),
    );
    assert_eq!(
        db_mutant_report.step(StepName::Test).unwrap().status,
        StepStatus::Failed,
        "GM + sidecar must catch DB overwrite regression:\n{db_mutant_report}"
    );

    // 4) Clean end-to-end DB + Golden Master pass through all gate steps & coverage (GM-08).
    let old_refund = r#"def apply_refund(external_id: str, refund_cents: int) -> int:
    # Bug GM-08: does not clamp refund_cents against remaining balance.
    query(
        f"UPDATE payment_orders "
        f"SET refunded_cents = refunded_cents + {refund_cents} "
        f"WHERE external_id = '{external_id}';"
    )
    rows = query(
        f"SELECT amount_cents - refunded_cents FROM payment_orders "
        f"WHERE external_id = '{external_id}';"
    )
    return int(rows[0][0] or "0")"#;
    let clean_refund = r#"def apply_refund(external_id: str, refund_cents: int) -> int:
    query(
        f"UPDATE payment_orders "
        f"SET refunded_cents = LEAST(amount_cents, refunded_cents + {refund_cents}) "
        f"WHERE external_id = '{external_id}';"
    )
    rows = query(
        f"SELECT amount_cents - refunded_cents FROM payment_orders "
        f"WHERE external_id = '{external_id}';"
    )
    return int(rows[0][0] or "0")"#;

    let clean_db = TempRepo::create("gm-08-clean", &gm_db_files);
    clean_db.write("tests/test_repro_gm08.py", ORDERS_REPRO_GM08);
    clean_db.replace("orders/repo.py", old_refund, clean_refund);
    let repro_gm08 = Repro {
        test: "tests/test_repro_gm08.py::test_refund_clamped_to_remaining_balance".into(),
        files: vec!["tests/test_repro_gm08.py".into()],
    };
    let (clean_report, clean_dur) = execute_gate(
        &clean_db,
        &prof,
        Some(&repro_gm08),
        Some(python_sandbox(&prof)),
        &[],
        &[],
        Some(sidecar_cfg),
    );
    assert!(
        clean_report.passed(),
        "GM-08 clean DB fix through full gate failed:\n{clean_report}"
    );
    assert_eq!(
        clean_report.step(StepName::Coverage).unwrap().status,
        StepStatus::Passed
    );

    let sc = clean_report
        .sidecars
        .expect("GM-08 must record SidecarReport");
    assert_eq!(sc.services, 1);
    assert!(
        sc.resets >= 3,
        "expected >= 3 resets (repro-base, test, coverage), got {}",
        sc.resets
    );
    assert!(sc.start_duration > Duration::ZERO);
    assert!(sc.reset_duration > Duration::ZERO);
    assert!(sc.teardown_duration > Duration::ZERO);

    let step_times: Vec<String> = clean_report
        .steps
        .iter()
        .map(|s| {
            format!(
                "{}={}ms({:?})",
                s.name.id(),
                s.duration.as_millis(),
                s.status
            )
        })
        .collect();
    eprintln!(
        "[gm-07-idempotent-payment] nonet_fail_ms={} baseline_ms={} (passed={}) gm_mutant_ms={} (caught={})",
        nonet_dur.as_millis(),
        db_baseline_dur.as_millis(),
        db_baseline_report.passed(),
        db_mutant_dur.as_millis(),
        !db_mutant_report.passed(),
    );
    eprintln!(
        "[gm-08-refund-balance] total_ms={} sidecars(start_ms={}, resets={}, reset_ms={}, teardown_ms={}, total_sidecar_ms={}) steps=[{}]",
        clean_dur.as_millis(),
        sc.start_duration.as_millis(),
        sc.resets,
        sc.reset_duration.as_millis(),
        sc.teardown_duration.as_millis(),
        sc.total_duration().as_millis(),
        step_times.join(", "),
    );
}
