# Tenant isolation on Kubernetes

On the `kubernetes` backend the Manager gives each OpenFlows organization one
namespace and walls it. It also proves at startup that the cluster enforces the
walls. The design is in [portable workspace runtime](../../architecture/portable-workspace-runtime.md#networking-and-isolation).

## What the Manager creates

When a tenant's first workspace credential is delivered (and again on every
rebuild) the Manager applies these objects. Repeating it is safe.

| Object | Purpose |
| --- | --- |
| Namespace `<OPENFLOWS_K8S_NAMESPACE_PREFIX><organization id>` | One per organization, enforcing the `restricted` Pod Security Standard |
| ResourceQuota `openflows-quota` | Ceiling on pods, CPU, memory and storage for the organization |
| LimitRange `openflows-defaults` | Default container requests and limits so pods fit the quota |
| ServiceAccount `default` | Stops pods that name no account from receiving a Kubernetes API token |
| NetworkPolicy `openflows-default-deny` | Denies all ingress and egress for every pod |
| NetworkPolicy `openflows-allow-dns` | Egress to CoreDNS (`k8s-app=kube-dns` in `kube-system`) |
| NetworkPolicy `openflows-allow-platform` | Egress to the platform namespace and any platform CIDRs, on the platform ports |
| NetworkPolicy `openflows-allow-public-egress` | Egress to the internet except private, link-local and carrier-grade NAT ranges |
| NetworkPolicy `openflows-tenant-<tenant id>` | Ingress and egress between pods of that tenant only |
| NetworkPolicy `openflows-provisioner` | Only when `OPENFLOWS_K8S_PROVISIONER_SELECTOR` is set |

Deleting a tenant removes its `openflows-tenant-*` policy. The organization
namespace stays.

## What workspace templates must do

- Label every pod `openflows.io/tenant=<tenant id>` and
  `openflows.io/organization=<organization id>`. A pod without the tenant label
  reaches nothing but DNS and the public internet.
- Satisfy the `restricted` standard: run as non-root, set
  `allowPrivilegeEscalation: false`, drop all capabilities, use the
  `RuntimeDefault` seccomp profile, and set `automountServiceAccountToken: false`.
- Reach other pods of the tenant (the Nexus relay Service) by name or pod IP.
  The same tenant is allowed, any other tenant is not.

## Settings

| Variable | Default | Meaning |
| --- | --- | --- |
| `OPENFLOWS_K8S_NAMESPACE_PREFIX` | `openflows-` | Prefix of organization namespaces |
| `OPENFLOWS_K8S_PLATFORM_NAMESPACE` | `openflows` | Namespace of the Manager, Coder and Redis |
| `OPENFLOWS_K8S_PLATFORM_EGRESS_PORTS` | `3002,6379,7080` | TCP ports pods may reach there |
| `OPENFLOWS_K8S_PLATFORM_EGRESS_CIDRS` | none | Platform services outside the cluster, such as a managed Redis |
| `OPENFLOWS_K8S_PROVISIONER_SELECTOR` | none | `key=value` label of the Coder provisioner pods |
| `OPENFLOWS_K8S_API_EGRESS_CIDRS` | none | Kubernetes API address, for the provisioner policy |
| `OPENFLOWS_K8S_PROVISIONER_SERVICE_ACCOUNT` | none | `namespace/name` of the provisioner's service account; see below |
| `OPENFLOWS_K8S_PROBE_IMAGE` | `docker.io/library/busybox:1.37.0` | Image with `sh` and `nc` for the startup probe; pin or mirror it |
| `OPENFLOWS_K8S_QUOTA_PODS`, `_REQUESTS_CPU`, `_REQUESTS_MEMORY`, `_LIMITS_CPU`, `_LIMITS_MEMORY`, `_PVCS`, `_STORAGE` | `50`, `20`, `64Gi`, `40`, `128Gi`, `50`, `500Gi` | Quota per organization namespace |

The default-deny rule covers every pod in the namespace, including a provisioner
running there. Set `OPENFLOWS_K8S_PROVISIONER_SELECTOR` so it keeps its access to
Coder and, with `OPENFLOWS_K8S_API_EGRESS_CIDRS`, to the Kubernetes API.

## Manager permissions

The Manager's identity needs, in addition to the Secret permissions described in
the [production runbook](operator-production-runbook.md):

```yaml
rules:
  - apiGroups: [""]
    resources: [namespaces]
    verbs: [get, create, patch, delete]   # delete only for the startup probe's throwaway namespace
  - apiGroups: [""]
    resources: [resourcequotas, limitranges, serviceaccounts]
    verbs: [get, create, patch]
  - apiGroups: [authorization.k8s.io]
    resources: [subjectaccessreviews]
    verbs: [create]                       # the provisioner check below
  - apiGroups: [networking.k8s.io]
    resources: [networkpolicies]
    verbs: [get, create, patch, delete]
  - apiGroups: [""]
    resources: [pods]
    verbs: [get, create, delete]          # startup probe only
```

Granting this cluster-wide lets the Manager create and wall namespaces. A
separate delivery identity for Secrets, as the design describes, is not split out yet.

## Provisioner access to Secrets

The runtime credentials of every tenant are Secrets in the organization
namespace, so the Coder provisioner must not be able to read them. The Manager
does not write the provisioner's permissions, since they belong to the
infrastructure. Set `OPENFLOWS_K8S_PROVISIONER_SERVICE_ACCOUNT` and each time a
tenant's namespace is set up the Manager asks the API server whether that account
can `get`, `list` or `watch` Secrets there. If it can, the tenant is not set up
and the error names the account and the verb. Without the setting no check runs.

## Startup probe

In hosted mode on Kubernetes the Manager refuses to start unless the cluster
enforces the rules. It creates a throwaway namespace `openflows-probe-<id>`,
applies the same rules, and starts a server for tenant A with three clients:

| Client | Must |
| --- | --- |
| Same tenant | Connect (the control that shows the probe works) |
| Other tenant | Fail |
| No tenant label | Fail |

The namespace is removed afterwards. Failures name the cause:

| Message | Fix |
| --- | --- |
| `... reached a pod of another. The cluster does not enforce NetworkPolicy` | Install a network plugin that enforces it (Calico, Cilium). Many clusters accept the objects and ignore them. On EKS the VPC CNI needs network policy enabled, or install Calico or Cilium |
| `... could not reach another pod of its own tenant` | The network plugin is unhealthy, or `OPENFLOWS_K8S_PROBE_IMAGE` lacks `nc` or `sh` |
| `probe pod ... did not start in time` | The cluster cannot pull the probe image, or pods cannot run in a `restricted` namespace. Mirror the image and set `OPENFLOWS_K8S_PROBE_IMAGE` |
| `cannot create a probe namespace` | The Manager lacks the permissions above |

The probe runs once at startup. Re-run it by restarting the Manager after any
change to the network plugin.

## On EKS

Nothing above is specific to EKS, but these points need attention there. They
were not verified on an EKS cluster.

- The default pod network does not enforce NetworkPolicy. Turn on network policy
  support in the VPC CNI add-on, or install Calico or Cilium. Where the
  organization denies add-on updates, use the second option. The startup probe
  tells you which one worked.
- Keep pods away from the instance metadata service. The network rules already
  block `169.254.169.254`; also require IMDSv2 with a hop limit of 1 on the nodes.
- Turn on KMS envelope encryption for Secrets, since runtime credentials are
  stored as Secrets.
- Pods using a secondary VPC range (the `100.64.0.0/10` block) are covered by the
  non-public ranges excluded from the public egress rule.

## Limits

- Pods share a node's kernel. Hostile code needs sandboxed pods or a VM backend.
- Egress is not limited by hostname. Standard NetworkPolicy cannot do that; use a
  plugin with FQDN policy or an egress proxy.
- The quota is per organization, not per tenant.
- Nothing may start a connection into a tenant pod from outside the namespace.
- Deleting an organization does not remove its namespace.

## Testing against a real cluster

The tests in `crates/openflows-manager/tests/k8s_isolation.rs` use the cluster of
the current kubeconfig context. With [kind](https://kind.sigs.k8s.io), create a
cluster without its default network plugin and install Calico:

```bash
printf 'kind: Cluster\napiVersion: kind.x-k8s.io/v1alpha4\nnetworking:\n  disableDefaultCNI: true\n  podSubnet: 192.168.0.0/16\n' > kind.yaml
kind create cluster --name of507 --config kind.yaml
kubectl apply -f https://raw.githubusercontent.com/projectcalico/calico/v3.33.0/manifests/calico.yaml
cargo test -p openflows-manager --test k8s_isolation enforcing -- --ignored --test-threads=1
```

`ignoring_cluster_fails_the_probe` needs a cluster whose plugin enforces nothing.
