# Security Policy

## Supported Versions

Security fixes are applied to the latest minor release series of `aegis-code`.

| Version | Supported |
| :--- | :---: |
| Latest (`0.17.x`) | ✅ |
| Older releases | ❌ |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues or pull requests.**

If you discover a security vulnerability in `aegis-code` (for example, a sandbox escape, `aegis-docker-proxy` policy bypass, prompt-injection control bypass, secret leakage, or authentication/authorization flaw):

1. Report it privately using **GitHub Security Advisories**:
   <https://github.com/hoanghainh1188/aegis-code/security/advisories/new>
2. Include as much of the following as possible to help us reproduce and remediate quickly:
   - Affected version or commit SHA and deployment flavor (Compose, systemd, AWS, Kubernetes);
   - Step-by-step reproduction instructions or a minimal proof of concept;
   - Potential impact and any suggested mitigation.

## Response Process

- **Acknowledgment:** Within **3 business days** of receiving the report.
- **Triage & Assessment:** Within **7 business days**, we will confirm severity and share an expected remediation plan.
- **Disclosure:** We coordinate disclosure with the reporter and publish a security advisory alongside the patched release.

## Security Architecture & Emergency Response

- For a full overview of Aegis's security controls, data residency, sandbox isolation, and prompt-injection defenses, see [docs/security.md](docs/security.md).
- For active operational incidents on a running instance (pausing all agents immediately, secret rotation, and recovery), see [docs/runbook/incident.md](docs/runbook/incident.md).
