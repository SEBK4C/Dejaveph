# Deployment templates

Copy-paste NixOS configs for the three-machine Dejaveph topology. Each is a self-contained
flake — initialise one with:

```bash
nix flake init -t github:SEBK4C/Dejaveph#<name>
```

| `#name` | Role | Runs | Secrets |
|---|---|---|---|
| `demo` | one box | xetd **local-fs** + a local xetfs mount | none |
| `gateway` | server | xetd on **Ceph RGW** (s3) + TLS proxy (TODO) | RGW keys via 1Password/opnix |
| `client` | client | `services.xetfs` mounts | `XETD_TOKEN` via 1Password/opnix |

The third machine — **storage** (Ceph MON/OSD/RGW) — is bring-your-own; a single-node `cephadm`
cluster is fine for a homelab. See [`../docs/DEPLOYMENT.md`](../docs/DEPLOYMENT.md) for the RGW
user/bucket bootstrap and the 1Password vault/item conventions.

## Recommended path

1. **Start with `demo`** — `nixos-rebuild switch --flake .#demo`. No Ceph, no secrets, no tokens.
   Proves the whole stack on one host.
2. **Grow to `gateway` + `client`** when you outgrow one disk. The only server change from `demo`
   is `backend = "s3"` + the RGW credentials block; everything else carries over.

## Canonical names used by the templates

vault `Infrastructure` · items `dejaveph-ceph-rgw` / `dejaveph-xetd-tokens` · host
`dejaveph.home.arpa` · RGW `https://rgw.ceph.home.arpa` · bucket `dejaveph-xorbs` · port `9777`.

> Note: opnix option names (`services.onepassword-secrets.*`) vary by release — verify against
> the opnix you pin. The Dejaveph modules only consume a `credentialsFile`/`tokenFile` path, so
> any secret manager (agenix, sops-nix, a hand-deployed file) works identically.
