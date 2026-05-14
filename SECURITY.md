# Security Policy

pidory is a Discord bot that bridges Discord threads to Claude Code CLI subprocesses. Its attack surface covers the Discord bot token, the PostgreSQL session database, Claude CLI subprocess tool permissions, and outbound forwarding of Discord user identifiers to the Anthropic API (see `CLAUDE.md` § Privacy / PII Forwarding, #316).

No secrets have been found in the repository history — this policy is preventive.

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.7.x   | ✅        |
| < 0.7   | ❌        |

## Reporting a Vulnerability

Please report security vulnerabilities **privately** — do not open a public GitHub issue.

1. **Preferred** — open a [GitHub Security Advisory](https://github.com/deokdory/pidory/security/advisories/new) (visible only to maintainers).
2. **Fallback** — email `deokdory@gmail.com` with the subject prefix `[pidory security]`.

Include as much detail as you can: steps to reproduce, potential impact, and any suggested mitigations.

## Response Timeline

This is a hobby project maintained by a single owner — best-effort, no SLA:

- **Acknowledgement**: within roughly one week of receiving the report.
- **Triage and fix planning**: reviewed on a quarterly basis alongside other security work.
- **Disclosure**: coordinated with the reporter once a fix is ready or a decision has been made.
