# Tenant isolation on Kubernetes

On the `kubernetes` backend the Manager gives each OpenFlows organization one
namespace and walls it. It also proves at startup that the cluster enforces the
walls. The design is in [portable workspace runtime](../../architecture/portable-workspace-runtime.md#networking-and-isolation).

## What the Manager creates

When a tenant's first workspace credential is delivered (and again on every
rebuild) the Manager applies these objects. Repeating it is safe.

| Object | Purpose |
| --- | --- |
| Namespace `<OPENFLOWS_K8S_NAMESPACE_PREFIX><organization id>` | One per organization, enforcing the `restricted` Pod Security Standard. `enforce` is pinned to a version (default `v1.33`) so a cluster upgrade cannot start rejecting pods; `audit` and `warn` follow the latest, so an upgrade shows up as warnings first |
| ResourceQuota `openflows-quota` | Ceiling on pods, CPU, memory and storage for the organization |
| LimitRange `openflows-defaults` | Default container requests and limits so pods fit the quota |
| ServiceAccount `default` | Stops pods that name no account from receiving a Kubernetes API token |
| NetworkPolicy `openflows-default-deny` | Denies all ingress and egress for every pod |
| NetworkPolicy `openflows-allow-dns` | Egress to CoreDNS (`k8s-app=kube-dns` in `kube-system`) and, when set, the node-local DNS cache |
| NetworkPolicy `openflows-allow-platform` | Egress for tenant-labelled pods to the platform namespace and any platform CIDRs, on the platform ports. A pod without the tenant label gets none of it |
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
| `OPENFLOWS_K8S_DELIVERY_KUBECONFIG` | none | Kubeconfig of the Secret delivery identity, apart from the namespace identity. Required in hosted mode; see below |
| `OPENFLOWS_K8S_DELIVERY_SERVICE_ACCOUNT` | none | `namespace/name` of that identity's service account. Required in hosted mode: each organization namespace grants it Secret write access |
| `OPENFLOWS_K8S_PROVISIONER_SELECTOR` | none | `key=value` label of the Coder provisioner pods |
| `OPENFLOWS_K8S_API_EGRESS_CIDRS` | none | Kubernetes API address, for the provisioner policy |
| `OPENFLOWS_K8S_PROVISIONER_SERVICE_ACCOUNT` | none | `namespace/name` of the provisioner's service account; see below |
| `OPENFLOWS_K8S_POD_SECURITY_VERSION` | `v1.33` | Pod Security Standard version that `enforce` is pinned to, or `latest`. Raise it deliberately after checking the `audit` and `warn` messages |
| `OPENFLOWS_K8S_PROBE_NAMESPACE` | none | Existing empty namespace for the startup probe; see below. Unset, the probe creates and deletes its own |
| `OPENFLOWS_K8S_NODE_LOCAL_DNS` | none | Address of the NodeLocal DNSCache listener, usually `169.254.20.10`. Set it on clusters that run NodeLocal DNSCache: pods query it instead of CoreDNS, and it sits in a range the public egress rule excludes |
| `OPENFLOWS_K8S_PROBE_IMAGE` | `docker.io/library/busybox:1.37.0` | Image with `sh` and `nc` for the startup probe; pin or mirror it |
| `OPENFLOWS_K8S_QUOTA_PODS`, `_REQUESTS_CPU`, `_REQUESTS_MEMORY`, `_LIMITS_CPU`, `_LIMITS_MEMORY`, `_PVCS`, `_STORAGE` | `50`, `20`, `64Gi`, `40`, `128Gi`, `50`, `500Gi` | Quota per organization namespace |

The default-deny rule covers every pod in the namespace, including a provisioner
running there. Set `OPENFLOWS_K8S_PROVISIONER_SELECTOR` so it keeps its access to
Coder and, with `OPENFLOWS_K8S_API_EGRESS_CIDRS`, to the Kubernetes API.

## The Manager's two identities

The Manager does two jobs that must not share an identity. In hosted mode it
refuses to start unless they are separate.

| Identity | Does | Must not be able to |
| --- | --- | --- |
| Namespace identity | Builds each organization's namespace, quota and rules, and binds the delivery identity | Read or write any Secret |
| Secret delivery identity | Writes the runtime Secrets | Administer namespaces, quotas, rules or bindings, or read Secrets |

Someone who can build namespaces then cannot read a tenant's credentials, and the
identity that writes them cannot change the walls around them.

**Namespace identity.** The Manager's own identity (in-cluster service account or
kubeconfig). It needs this cluster role:

```yaml
rules:
  - apiGroups: [""]
    resources: [namespaces]
    verbs: [get, create, patch]
  - apiGroups: [""]
    resources: [resourcequotas, limitranges, serviceaccounts]
    verbs: [get, create, patch]
  - apiGroups: [networking.k8s.io]
    resources: [networkpolicies]
    verbs: [get, create, patch, delete]   # delete removes a deleted tenant's rule
  - apiGroups: [authorization.k8s.io]
    resources: [subjectaccessreviews]
    verbs: [create]                       # the account checks below
  - apiGroups: [rbac.authorization.k8s.io]
    resources: [rolebindings]
    verbs: [get, create, patch]           # binds the delivery identity per namespace
  - apiGroups: [rbac.authorization.k8s.io]
    resources: [clusterroles]
    verbs: [bind]
    resourceNames: [openflows-secret-writer]
```

**Secret delivery identity.** A second service account, for example
`openflows/secret-delivery`, with no binding of its own. Create this cluster role
once; nobody binds it cluster-wide:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: openflows-secret-writer
rules:
  - apiGroups: [""]
    resources: [secrets]
    verbs: [create, patch, delete]
```

Set `OPENFLOWS_K8S_DELIVERY_SERVICE_ACCOUNT=openflows/secret-delivery`. For each
organization namespace the namespace identity then creates a RoleBinding
`openflows-secret-delivery` giving that account the role there, so it can write
Secrets in organization namespaces and nowhere else. It cannot read them.

Give the Manager a kubeconfig for that account in
`OPENFLOWS_K8S_DELIVERY_KUBECONFIG`, for example with a projected token:

```yaml
apiVersion: v1
kind: Config
clusters:
  - name: cluster
    cluster:
      server: https://kubernetes.default.svc
      certificate-authority: /var/run/secrets/kubernetes.io/serviceaccount/ca.crt
users:
  - name: delivery
    user:
      tokenFile: /var/run/secrets/openflows-delivery/token
contexts:
  - name: delivery
    context: { cluster: cluster, user: delivery }
current-context: delivery
```

**What the Manager checks at startup in hosted mode**
- The kubeconfig and the service account are both set.
- The kubeconfig authenticates as that service account.
- The namespace identity cannot `get`, `list`, `watch`, `create`, `update`, `patch`
  or `delete` Secrets.
- The delivery identity cannot create or change namespaces, network policies,
  quotas, limit ranges, service accounts or role bindings.

Each time a namespace is set up, the Manager also asks the API server whether the
delivery account can create, patch and delete Secrets there, and refuses if not.
In local mode with a single identity none of this applies.

### What the startup probe needs

The namespace identity needs more for the startup probe, in one of two ways.

**Recommended: a probe namespace.** Create an empty namespace once, labelled
`pod-security.kubernetes.io/enforce=restricted`, set
`OPENFLOWS_K8S_PROBE_NAMESPACE` to its name, and grant the namespace identity this Role
in that namespace only. The probe runs there and removes its own pods and rules, so
the Manager never needs to create or delete a namespace for it:

```yaml
rules:
  - apiGroups: [""]
    resources: [pods]
    verbs: [get, create, delete]
  - apiGroups: [networking.k8s.io]
    resources: [networkpolicies]
    verbs: [get, create, patch, delete]
```

The two probes then run one after the other, and the namespace is meant for one
Manager at a time. Start replicas one by one, or give each its own namespace.

**Default: throwaway namespaces.** With the setting unset the probe creates and
deletes two namespaces itself. That needs `delete` on `namespaces` and `get` and
`create` on `pods`, all cluster-wide. Because `delete` cannot be limited to the
probe's namespaces, it lets the Manager delete any namespace in the cluster. Use
the probe namespace instead wherever that matters.

Granting the cluster role lets the Manager create and wall namespaces.

## Provisioner access to Secrets

The runtime credentials of every tenant are Secrets in the organization
namespace, so the Coder provisioner must not be able to read them. The Manager
does not write the provisioner's permissions, since they belong to the
infrastructure. Set `OPENFLOWS_K8S_PROVISIONER_SERVICE_ACCOUNT` and each time a
tenant's namespace is set up the Manager asks the API server whether that account
can `get`, `list` or `watch` Secrets there, counting the groups every service
account belongs to (`system:serviceaccounts`, `system:serviceaccounts:<namespace>`,
`system:authenticated`). If it can, the tenant is not set up
and the error names the account and the verb. Without the setting no check runs,
and in hosted mode the Manager logs a warning at startup saying so.

**Limit.** This check closes one route: reading Secrets through the Kubernetes
API. It does not stop the provisioner from reaching them another way. The
provisioner creates the workspace pods, and Kubernetes lets whoever creates a pod
name any Secret in the pod's namespace and mount it as a file. It only checks that
the Secret is in the same namespace, not whether the creator may read it. A
provisioner could therefore create a pod that mounts another tenant's
`openflows-runtime-<workspace id>` Secret and read it from inside that pod. Today
the protection against this rests on the workspace templates being ours and
pinned, and on Coder and its provisioner not being compromised.

**Decision needed.** Closing this route is a decision for the team, because each
way has a cost. An admission policy stops a pod mounting the wrong Secret, but the
label it relies on is set by whoever creates the pod. Delivering credentials
without Kubernetes Secrets in the tenants' namespace changes how every credential
is delivered and overlaps #500. Until the team decides, this stays a documented
limit.

## Startup probe

In hosted mode on Kubernetes the Manager refuses to start unless the cluster
enforces the rules. It runs two probes, each in its own throwaway namespace and
at the same time, or one after the other in the probe namespace if one is set.

The tenant probe applies the same rules and starts a server for tenant A with
three clients:

| Client | Must |
| --- | --- |
| Same tenant | Connect (the control that shows the probe works) |
| Other tenant | Fail |
| No tenant label | Fail |

A pod-to-pod connection needs the source's egress rules and the destination's
ingress rules to both allow it, so one blocked connection does not say which half
is enforced. The direction probe tests each half alone, with a free server and
free client that no rule touches:

| Connection | Must |
| --- | --- |
| Free client to free server | Connect (the control) |
| Free client to a server guarded only by an ingress rule | Fail |
| Client guarded only by an egress rule to the free server | Fail |

The throwaway namespaces are removed afterwards; a probe namespace is left empty.
Failures name the cause:

| Message | Fix |
| --- | --- |
| `... reached a pod of another. The cluster does not enforce NetworkPolicy` | Install a network plugin that enforces it (Calico, Cilium). Many clusters accept the objects and ignore them. On EKS the VPC CNI needs network policy enabled, or install Calico or Cilium |
| `a pod guarded by an ingress rule was still reachable` or `... egress rule could still connect out` | The plugin enforces only one direction of NetworkPolicy, or none of it. Check its policy settings, or install Calico or Cilium |
| `a pod with no rules could not reach another pod with no rules` | The network plugin is unhealthy |
| `... could not reach another pod of its own tenant` | The network plugin is unhealthy, or `OPENFLOWS_K8S_PROBE_IMAGE` lacks `nc` or `sh` |
| `probe pod ... did not start in time` | The cluster cannot pull the probe image, or pods cannot run in a `restricted` namespace. Mirror the image and set `OPENFLOWS_K8S_PROBE_IMAGE` |
| `cannot create a probe namespace` | The Manager lacks the permissions above, or set `OPENFLOWS_K8S_PROBE_NAMESPACE` to a namespace that exists |
| `the probe namespace '...' cannot be used` | The namespace in `OPENFLOWS_K8S_PROBE_NAMESPACE` does not exist, or the Manager cannot read it |

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
