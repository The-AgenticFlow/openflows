//! Tenant isolation against a real Kubernetes cluster.
//!
//! These tests use the cluster in the current kubeconfig context, so they are
//! ignored by default. See docs/implementation/centralized-deployment/tenant-isolation.md.
//!
//! On a cluster whose network plugin enforces NetworkPolicy (for example kind
//! with Calico):
//!   `cargo test --test k8s_isolation enforcing -- --ignored --test-threads=1`
//! On one that does not (for example kind with its default plugin):
//!   `cargo test --test k8s_isolation ignoring -- --ignored --test-threads=1`

mod binding_rule;
mod identities;
mod support;
mod walls;
