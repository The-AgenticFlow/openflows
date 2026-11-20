<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./assets/brand/openlogo-light.png">
    <source media="(prefers-color-scheme: light)" srcset="./assets/brand/openlogo-dark.png">
    <img src="./assets/brand/openlogo-dark.png" alt="" width="48">
  </picture>
  OpenFlows
</h1>

<h2 align="center">A self-hosted AI development team for GitHub</h2>

<p align="center"><strong>Turn GitHub issues into reviewed pull requests on your own infrastructure.</strong></p>

<hr>

<p align="center">
  <strong>Self-host with</strong> <a href="#deployment"><img src="./assets/cloud-platforms/docker.svg" alt="Docker" width="44"></a> <strong>or</strong>
  <a href="#deployment"><img src="./assets/cloud-platforms/kubernetes.svg" alt="Kubernetes" width="44"></a> <strong>on</strong>&nbsp;
  <a href="#deployment"><img src="./assets/cloud-platforms/aws.svg" alt="AWS" width="48"></a>&nbsp;&nbsp;
  <a href="#deployment"><img src="./assets/cloud-platforms/azure.svg" alt="Azure" width="48"></a>&nbsp;&nbsp;
  <a href="#deployment"><img src="./assets/cloud-platforms/google-cloud.svg" alt="Google Cloud" width="48"></a>
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> · <a href="#deployment">Deployment</a> · <a href="#how-it-works">How it works</a> · <a href="#contribute">Contribute</a> · <a href="#documentation">Docs</a> · <a href="https://openflows.dev">Website</a>
</p>

<p align="center">
  <a href="https://github.com/The-AgenticFlow/openflows/releases/latest"><img src="https://img.shields.io/github/v/release/The-AgenticFlow/openflows?style=flat-square" alt="Latest release"></a>
  <a href="https://github.com/The-AgenticFlow/openflows/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/The-AgenticFlow/openflows/ci.yml?branch=develop&amp;label=checks&amp;style=flat-square" alt="CI checks"></a>
  <a href="https://github.com/The-AgenticFlow/openflows/stargazers"><img src="https://img.shields.io/github/stars/The-AgenticFlow/openflows?style=flat-square" alt="GitHub stars"></a>
  <a href="https://github.com/The-AgenticFlow/openflows/graphs/contributors"><img src="https://img.shields.io/github/contributors/The-AgenticFlow/openflows?style=flat-square" alt="Contributors"></a>
  <a href="https://github.com/The-AgenticFlow/openflows/issues"><img src="https://img.shields.io/github/issues/The-AgenticFlow/openflows?style=flat-square" alt="Open issues"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/The-AgenticFlow/openflows?style=flat-square" alt="MIT license"></a>
</p>

OpenFlows coordinates AI agents inside [Coder](https://coder.com) workspaces. A controller assigns work, checks plans before coding begins, sends changes through review and CI, and recovers interrupted runs. Coder provides the workspaces and model access; OpenFlows manages the delivery workflow.

- **From issue to merge:** agents plan, implement, review, and merge changes after the required checks pass.
- **Review before code:** the reviewer approves the implementation plan before the builder starts.
- **Run on your infrastructure:** Coder hosts the workspaces, while OpenFlows keeps coordination and audit records in its shared store.
- **Serve multiple repositories:** each tenant has its own repository binding, controller workspace, and Redis namespace.

<img src="./assets/home.png" alt="OpenFlows demo" style="width: 100%; max-width: 1200px; height: auto; display: block; margin: 0 auto;">

## Quickstart

You need Docker, `curl`, `python3`, access to a GitHub repository where you can install the OpenFlows App, and an API key for a supported model provider. Building from source also requires Rust.

From a checkout of this repository, run:

```bash
./scripts/setup.sh owner/repo --binary
```

Use `--compose` for published container images or `--source` to build from this checkout. The setup script starts the local services, opens the Manager at <http://localhost:3002> to connect GitHub, configures your model, and creates the workspace templates and repository tenant. It can be rerun after an interrupted setup.

Then open an issue in the connected repository. OpenFlows will pick it up and work toward a reviewed pull request. Check the local Coder dashboard at <http://localhost:7080> or run `./scripts/prod.sh doctor` to inspect the installation.

For installation modes, platform requirements, and the complete walkthrough, see the [quick start guide](quick_start.md). For upgrades and troubleshooting, see the [setup reference](docs/setup-reference.md).

## Deployment

OpenFlows is designed to run on infrastructure you control. The deployment path depends on the workspace backend and the isolation you need:

| Environment | Deployment path |
| --- | --- |
| **Local machine or single cloud VM** | Run the Docker-based setup above. It brings up Coder, the Manager, Redis, and workspaces on one host. This is the working development and evaluation path, including on a Docker-capable VM in AWS, Azure, Google Cloud, or another provider. It is not a public multi-tenant production deployment. |
| **AWS with Kubernetes (EKS)** | The Kubernetes backend includes pod-based templates for all five agents, per-workspace Secrets, and tenant isolation controls. A hosted deployment needs separate staging and production environments, workload IAM, AWS Secrets Manager, and the release validation in the AWS runbook before production use. |
| **Other Kubernetes providers** | The pod templates are provider-neutral, but a non-AWS hosted secret-store adapter and release validation are still required before that path is supported. |

The [operator deployment guide](docs/implementation/centralized-deployment/operator-deployment.md) explains the deployment profiles. See the [portable workspace runtime design](docs/architecture/portable-workspace-runtime.md) for the Kubernetes architecture and the [AWS runbook](docs/implementation/centralized-deployment/operator-production-runbook.md) for its prerequisites and release gates.

## How it works

```text
GitHub issue → NEXUS assigns → FORGE plans → SENTINEL approves the plan
             → FORGE implements → SENTINEL reviews → VESSEL checks CI and merges
```

| Agent | Responsibility |
| --- | --- |
| **NEXUS** | Coordinates the run and recovers stalled work. |
| **FORGE** | Implements the approved plan and opens a pull request. |
| **SENTINEL** | Reviews the plan and code, including security and tests. |
| **VESSEL** | Watches CI, resolves merge conflicts, and merges approved work. |
| **LORE** | Maintains documentation and project history when enabled. |

OpenFlows asks for human input when a decision needs it. The full design is in the [system architecture](docs/architecture/openflows-system-architecture.md).

## Contribute

You can help improve what each agent knows and how it works. The separate [workspace-runtime repository](https://github.com/The-AgenticFlow/workspace-runtime) owns the instructions, skills, commands, hooks, and standards for NEXUS, FORGE, SENTINEL, VESSEL, and LORE. Contribute agent-specific knowledge under [`roles/<role>/`](https://github.com/The-AgenticFlow/workspace-runtime/tree/main/roles), or knowledge shared by every agent under [`shared/`](https://github.com/The-AgenticFlow/workspace-runtime/tree/main/shared). Follow its [contribution guide](https://github.com/The-AgenticFlow/workspace-runtime/blob/main/CONTRIBUTING.md) when opening a pull request.

## Documentation

| Guide | Use it for |
| --- | --- |
| [Quick start](quick_start.md) | Prerequisites and first run. |
| [Setup reference](docs/setup-reference.md) | Configuration, upgrades, and troubleshooting. |
| [System architecture](docs/architecture/openflows-system-architecture.md) | Components and workflow. |
| [Controller](docs/architecture/openflows-controller.md) | Orchestration and recovery. |
| [Worker workspaces](docs/architecture/openflows-worker-workspace.md) | Workspace setup and agent execution. |
| [Shared store](docs/architecture/openflows-redis-shared-store.md) | State and tenant isolation. |
| [A2A verification](docs/architecture/openflows-a2a-relay.md) | Delegated acceptance tests. |

## Support and security

[Open an issue](https://github.com/The-AgenticFlow/openflows/issues) for bugs and feature requests. To report a vulnerability, follow the [security policy](SECURITY.md).

## License

[MIT](LICENSE).
