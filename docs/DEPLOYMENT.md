# Deploying Dejaveph — NixOS + 1Password secrets + Ceph (plug-and-play)

This guide stands up `xetd` on a Ceph RGW (S3) backend on NixOS, with the RGW credentials kept
in 1Password and delivered to the service at runtime (never written to the Nix store).

- NixOS module: [`nixos/module.nix`](../nixos/module.nix) → `services.xetd.*`
- Worked host:  [`nixos/example.nix`](../nixos/example.nix)
- Flake outputs: `nixosModules.default`, `packages.xetd` (local-fs), `packages.xetd-s3` (Ceph/S3)

---

## 0. Naming conventions (pick once, reuse everywhere)

Consistent names make the fleet self-documenting and keep `op://` references stable.

| Thing | Suggested value | Notes |
|---|---|---|
| 1Password **vault** | `Infrastructure` | or `Homelab` if you separate personal/infra |
| RGW credential **item** | `dejaveph-ceph-rgw` | fields: `access_key_id`, `secret_access_key` |
| opnix **service account** | `op://Infrastructure/opnix-service-account/credential` | read-only, scoped to `Infrastructure` |
| (future) xetd **bearer tokens** | `dejaveph-xetd-tokens` | fields: `read_token`, `write_token` |
| xetd **host** | `dejaveph.home.arpa` | `home.arpa` is the RFC 8375 homenet zone — no DNS hijack risk |
| RGW **endpoint** | `https://rgw.ceph.home.arpa` | front RGW with TLS; xetd speaks plain HTTP itself |
| **bucket** | `dejaveph-xorbs` | immutable, content-addressed xorb objects |
| Ceph **RGW user** | `dejaveph` | dedicated, least-privilege user for this bucket |
| mount host(s) | `dejaveph-mnt-<role>.home.arpa` | e.g. `dejaveph-mnt-ml01` for an ML node |

`op://` reference shape used below:
`op://<vault>/<item>/<field>` → `op://Infrastructure/dejaveph-ceph-rgw/access_key_id`.

---

## 1. Ceph: create the RGW user, keys, and bucket

On a Ceph admin host (or via `cephadm shell`):

```bash
# Dedicated least-privilege RGW user for Dejaveph.
radosgw-admin user create \
  --uid=dejaveph \
  --display-name="Dejaveph Xet CAS" \
  --max-buckets=8

# -> prints "access_key" and "secret_key". Put them in 1Password (step 2).

# Create the bucket (path-style; xetd uses --s3-path-style by default for RGW).
AWS_ACCESS_KEY_ID=<access> AWS_SECRET_ACCESS_KEY=<secret> \
  aws --endpoint-url https://rgw.ceph.home.arpa \
      s3 mb s3://dejaveph-xorbs

# Optional: a lifecycle rule is NOT wanted — xorbs are immutable + content-addressed and
# garbage-collected by xetd's own mark-sweep GC (§11.1). Leave bucket lifecycle empty.
```

Notes
- **Path-style** addressing avoids `bucket.rgw.…` virtual-host DNS; it's the RGW-safe default
  and what `services.xetd.s3.pathStyle = true` sets.
- xorbs are immutable and deduplicated, so no versioning/lifecycle is needed on the bucket.
- Scope the user to just this bucket; do not reuse cluster-admin keys.

## 2. 1Password: store the RGW keys

Create item `dejaveph-ceph-rgw` in vault `Infrastructure` with two fields:

```
access_key_id      = <radosgw access_key>
secret_access_key  = <radosgw secret_key>
```

CLI form:

```bash
op item create --vault Infrastructure --title dejaveph-ceph-rgw --category 'API Credential' \
  'access_key_id[text]=<access>' \
  'secret_access_key[password]=<secret>'
```

Create a **service account** scoped read-only to `Infrastructure` and save its token to the
host at `/etc/opnix-token` (mode 0400, out-of-band — e.g. via your deploy tool, not in git).

## 3. NixOS: enable the module

Wire the flake inputs and import the module + the worked example:

```nix
{
  inputs.dejaveph.url = "github:SEBK4C/Dejaveph";
  inputs.opnix.url    = "github:brizzbuzz/opnix";

  # nixosConfigurations.dejaveph = nixpkgs.lib.nixosSystem {
  #   modules = [
  #     dejaveph.nixosModules.default
  #     opnix.nixosModules.default
  #     ./nixos/example.nix     # adjust endpoint/bucket/host to your fleet
  #   ];
  # };
}
```

`nixos/example.nix` does three things: (1) opnix renders the RGW creds to
`/run/secrets/dejaveph-ceph-rgw.env` (tmpfs, 0400) from the `op://` references; (2) `xetd` runs
on the `s3` backend reading that file via a systemd credential; (3) ordering ensures the secret
exists before `xetd` starts.

Deploy:

```bash
nixos-rebuild switch --flake .#dejaveph --target-host root@dejaveph.home.arpa
```

## 4. Verify

```bash
systemctl status xetd
journalctl -u xetd -f          # expect: "xetd starting backend=S3 …" then "listening addr=…"

# The RGW creds must NOT appear in the unit environment or the store:
systemctl show xetd -p Environment        # -> no AWS_* keys
grep -r AWS_SECRET /nix/store/ ; echo "exit=$? (1 = good, nothing in store)"
```

A successful xorb upload lands an object under `dejaveph-xorbs/xorbs/<h0h1>/<h2h3>/<hash>`.

## 5. Mounting the VFS on a client

The `xetfs` CLI (flake `packages.xetfs`) mounts a volume as a reconstructing FUSE filesystem.
Needs `/dev/fuse` + the setuid `fusermount` from `pkgs.fuse`.

```bash
# read-only
xetfs --server https://dejaveph.home.arpa --volume models /mnt/models
# read-write (write-back on close); tokens-mode server needs XETD_TOKEN in the environment
XETD_TOKEN=write-… xetfs --server https://dejaveph.home.arpa --volume scratch --rw /mnt/scratch
```

Declaratively on NixOS via the `xetfs` module (the client half), with the mount's bearer token
sourced from 1Password just like the server's RGW creds:

```nix
{ pkgs, dejaveph, ... }:
{
  imports = [ dejaveph.nixosModules.xetfs ];
  services.xetfs = {
    package = dejaveph.packages.${pkgs.system}.xetfs;
    mounts = {
      models = {
        server = "https://dejaveph.home.arpa";
        volume = "models";
        mountpoint = "/mnt/models";          # read-only
      };
      scratch = {
        server = "https://dejaveph.home.arpa";
        volume = "scratch";
        mountpoint = "/mnt/scratch";
        readWrite = true;
        tokenFile = "/run/secrets/dejaveph-xetd-token.env";  # XETD_TOKEN=… rendered by opnix
      };
    };
  };
}
```

For the token, add a `dejaveph-xetd-tokens` item to the `Infrastructure` vault and have opnix
render `XETD_TOKEN=op://Infrastructure/dejaveph-xetd-tokens/write_token` to that file.

---

## Security posture recap (matches the in-code hardening)

- **TLS:** xetd speaks plain HTTP, so the `gateway` template binds it to `127.0.0.1` and fronts
  it with **caddy** (TLS, `https://dejaveph.home.arpa`) — bearer tokens never hit the wire in
  cleartext. With the s3 backend this is clean: reconstruction returns presigned **RGW** URLs, so
  clients only call the small JSON API through caddy and fetch bulk bytes from RGW directly (also
  HTTPS). The `xetfs`/agent clients are built with `rustls-tls` so they can use `https://`. Use
  `tls internal` (Caddy's CA) for the `home.arpa` zone, or ACME for a public domain.
- **Secrets:** RGW keys live in 1Password, render to tmpfs via opnix, and reach xetd through a
  systemd credential — never the Nix store, never `ps`/journal.
- **Least privilege:** dedicated `dejaveph` RGW user scoped to one bucket; the systemd unit runs
  under a stable system user with `ProtectSystem=strict`, `PrivateDevices`, a syscall filter,
  and `MemoryDenyWriteExecute`.
- **Auth mode:** use `auth = "tokens"` for any non-loopback bind.
