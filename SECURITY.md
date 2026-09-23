# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for security reports.

Use GitHub's private vulnerability reporting:

1. Go to this repository's **Security** tab.
2. Click **Report a vulnerability**.
3. Describe the issue, impact, and reproduction steps.

We will acknowledge receipt as soon as possible. This is a small
beta-stage project maintained in spare time — expect a best-effort
response, not an SLA.

## Scope

Things we definitely want to hear about:

- **E2EE / cryptography** — `herdr-e2ee-v2` handshake (P-256, HKDF),
  AES-GCM frame sealing, key derivation, nonce handling.
- **Pairing** — invitation redemption, credential storage, device
  authorization.
- **Relay auth** — token handling, session isolation between paired
  devices, anything that crosses the trust boundary between phone and
  host.
- **Herdr boundary** — the Unix-socket API integration and what a
  compromised relay could reach.

Lower-severity but still welcome: dependency vulnerabilities, insecure
defaults in packaged scripts, leaked data in logs.

Out of scope: vulnerabilities in Herdr itself (report upstream to
[0cv/herdr](https://github.com/0cv/herdr)), Tailscale, or Android.

## Status

This project is **beta** software with no compatibility guarantees yet.
Deploy it accordingly — the supported remote transport is Tailscale,
which keeps the relay off the public internet; keep it that way.
