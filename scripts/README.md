# Helper scripts

Quality-of-life tooling for deploying and operating Dejaveph. Both are dependency-light bash.

| Script | What it does |
|---|---|
| [`dejaveph-doctor.sh`](dejaveph-doctor.sh) | Preflight a client/server host: `/dev/fuse`, `fusermount`, the `xetd`/`xetfs` binaries, server reachability, and `XETD_TOKEN`. Exit code = number of failed checks (composes in CI / `ExecStartPre`). |
| [`bootstrap-ceph.sh`](bootstrap-ceph.sh) | One command to provision the Ceph RGW user + bucket Dejaveph needs and (optionally) stash the keys in 1Password for opnix. Run on a Ceph admin node. |

```bash
# Preflight a mount host before mounting:
scripts/dejaveph-doctor.sh --server http://dejaveph.home.arpa:9777 --volume models

# Provision Ceph + 1Password in one shot (on a Ceph admin node):
scripts/bootstrap-ceph.sh \
  --endpoint https://rgw.ceph.home.arpa \
  --bucket dejaveph-xorbs --uid dejaveph \
  --vault Infrastructure --item dejaveph-ceph-rgw
```

These pair with the deployment templates (`templates/`) and the runbook (`docs/DEPLOYMENT.md`):
`bootstrap-ceph` fills the prerequisites the `gateway` template expects; `dejaveph-doctor` is the
"is it working?" check for the `client` template.
