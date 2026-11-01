# Coder Premium features used by OpenFlows

| Premium feature | How OpenFlows uses it |
| --- | --- |
| External provisioners | OpenFlows runs a dedicated Coder provisioner for each customer organization to process template imports and workspace builds. |
| Multi-organization access control | OpenFlows places each customer in a separate Coder organization and applies organization-specific access rules. |
| Service accounts and organization roles | OpenFlows creates machine identities and assigns scoped roles so it can provision customer resources without using a person's Coder account for every operation. |

See Coder's [plan comparison](https://coder.com/pricing), [external provisioner documentation](https://coder.com/docs/install/operate/provisioners), and [organization roles documentation](https://coder.com/docs/admin/users/groups-roles/).
