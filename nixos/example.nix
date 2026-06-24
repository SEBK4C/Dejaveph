# Example host: `xetd` on a Ceph RGW backend, with the RGW credentials sourced from 1Password
# via opnix (https://github.com/brizzbuzz/opnix). Drop into your flake's nixosConfigurations.
#
#   inputs.dejaveph.url = "github:SEBK4C/Dejaveph";
#   inputs.opnix.url    = "github:brizzbuzz/opnix";
#
#   nixosConfigurations.dejaveph = nixpkgs.lib.nixosSystem {
#     modules = [
#       dejaveph.nixosModules.default
#       opnix.nixosModules.default
#       ./nixos/example.nix
#     ];
#   };
#
# Suggested naming (keep these consistent across your fleet — see ../docs/DEPLOYMENT.md):
#   1Password vault : Infrastructure
#   item            : dejaveph-ceph-rgw   (fields: access_key_id, secret_access_key)
#   service-account : op://Infrastructure/opnix-service-account/credential
#   host            : dejaveph.home.arpa        (the xetd box)
#   RGW endpoint    : https://rgw.ceph.home.arpa
{ config, pkgs, dejaveph, ... }:
{
  # 1) opnix: authenticate to 1Password with a service-account token and render the RGW creds
  #    to a runtime EnvironmentFile (tmpfs, root-readable, never in the Nix store).
  #
  #    NOTE: opnix's exact option names (`services.onepassword-secrets.*`, `format`, `reference`)
  #    vary across releases — verify against the opnix version you pin. The xetd module itself
  #    only consumes a `credentialsFile` path, so ANY secret manager works: swap this block for
  #    agenix/sops-nix, or a manually-deployed `/run/secrets/dejaveph-ceph-rgw.env`, and the rest
  #    is unchanged. The required file is a dotenv with AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY.
  services.onepassword-secrets = {
    enable = true;
    tokenFile = "/etc/opnix-token";                     # service-account token, deployed out-of-band
    secrets.dejaveph-rgw = {
      path = "/run/secrets/dejaveph-ceph-rgw.env";
      mode = "0400";
      # opnix renders `op` references; we want a dotenv the systemd credential can source.
      reference = ''
        AWS_ACCESS_KEY_ID=op://Infrastructure/dejaveph-ceph-rgw/access_key_id
        AWS_SECRET_ACCESS_KEY=op://Infrastructure/dejaveph-ceph-rgw/secret_access_key
      '';
      format = "dotenv";
    };
  };

  # 2) xetd on the Ceph RGW backend, creds delivered by opnix above.
  services.xetd = {
    enable = true;
    package = dejaveph.packages.${pkgs.system}.xetd-s3;   # MUST be the s3-featured build
    listen = "0.0.0.0:9777";
    openFirewall = true;
    durability = "fsync";
    auth = "tokens";
    backend = "s3";
    s3 = {
      endpoint = "https://rgw.ceph.home.arpa";
      bucket = "dejaveph-xorbs";
      pathStyle = true;
      credentialsFile = "/run/secrets/dejaveph-ceph-rgw.env";
    };
  };

  # 3) Make sure the secret is rendered before xetd starts.
  systemd.services.xetd.after = [ "onepassword-secrets.service" ];
  systemd.services.xetd.wants = [ "onepassword-secrets.service" ];
}
