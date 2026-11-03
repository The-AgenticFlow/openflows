# OpenFlows Security

OpenFlows welcomes reports from security researchers and the community. If you believe you have found a vulnerability, we want to hear from you. This policy explains how to report it and what you can expect from us.

## Supported versions

Only the latest release receives security fixes.

## Where should I report security issues?

**Do not report security vulnerabilities through public issues, Discord, or pull requests.**

Instead, [submit a private vulnerability report](https://github.com/The-AgenticFlow/openflows/security/advisories/new) on GitHub. Only you and the maintainers will see your report.

Include the affected version, steps to reproduce, and the impact. The more detail you provide, the faster we can triage and fix the issue.

## Out of scope

Our primary concern is abuse of OpenFlows that exposes credentials, breaks tenant isolation, or bypasses its review and approval gates.

- **Vulnerabilities in Coder or other third-party systems.** Report these to the appropriate vendor.
- **Compromised credentials or hosts.** Attacks that rely on credentials or administrator access obtained outside OpenFlows.
- **Volumetric DoS/DDoS attacks.** Attacks that rely on traffic volume are the responsibility of the installation owner.

## Our commitments

- Acknowledge your report within 3 working days and work with you to validate it.
- Keep you informed as we investigate and fix the issue.
- Publish a security advisory once a fix is released, and credit you unless you prefer to remain anonymous.

## Our expectations

- Give us a reasonable amount of time (at least 90 days) to fix the issue before disclosing it publicly.
- Test only against installations you own or have permission to test.
- Avoid accessing other people's data, disrupting systems, or destroying data.
