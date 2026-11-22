# Security Policy

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security problems,
especially anything that could let an attacker drain, freeze, or
misallocate escrowed funds.

Report privately through either channel:

- **GitHub Security Advisories** — use the ["Report a vulnerability"](https://github.com/Arbiter-xyz/arbiter-contract/security/advisories/new)
  button on this repository's **Security** tab (preferred: it keeps the
  report, discussion and fix private until a coordinated disclosure).
- **Email** — `security@arbiter.xyz`.

Please include enough to reproduce: the entry point(s) involved, the exact
call sequence, and the fund-safety or authorization impact you believe it
has. If you have a proof-of-concept test (a Soroban test harness
reproduction is ideal), attach it.

## What's in scope

This repository — the Arbiter Soroban contract — is the highest-stakes
security surface in the project. In particular:

- Fund-safety of the settlement paths: `resolve()`, `resolve_by_consensus()`,
  `escalate()`/`escalate_resolve()`, `resolve_challengeable()`/
  `dispute_resolve()`/`finalize_resolve()`, `refund()`/`refund_timeout()`.
- Authorization gating: every state-mutating entry point's `require_auth()`/
  `require_admin()`/`require_resolver_or_admin()` checks, and the
  `initialize()`/admin-rotation/upgrade-proposal flows.
- Arithmetic correctness in fee, slash, and stake-maturity computations.
- The upgrade mechanism itself (`propose_upgrade()`/`execute_upgrade()`) and
  whether in-flight questions' fund-safety can depend on trusting new code.

Out of scope: the backend (report to
[arbiter-backend](https://github.com/Arbiter-xyz/arbiter-backend)), the
frontend (report to
[arbiter-app](https://github.com/Arbiter-xyz/arbiter-app)), and findings
that require a compromised admin key or already-leaked secret (that's a key-
management incident, not a contract bug).

## Deployment status

This is presently a **testnet-only** deployment. It is production-shaped
and the flows are live, but the keys and funds involved are testnet —
please calibrate severity accordingly. We would still rather hear about an
issue early than read it in a public issue.

## What to expect

We aim to acknowledge a report within **3 business days** and to give an
initial assessment (in scope / severity / rough timeline) within **7
business days**. We'll keep you updated as we work on a fix and are happy
to credit you in the advisory unless you'd prefer otherwise.
