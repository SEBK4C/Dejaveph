# Dejaveph GATEWAY host — runs xetd against a Ceph RGW backend, secrets from 1Password.
#
#   nix flake init -t github:SEBK4C/Dejaveph#gateway
#
# Edit: hostName, the RGW endpoint/bucket, and the op:// references. Then:
#   nixos-rebuild switch --flake .#gateway --target-host root@dejaveph.home.arpa
# Prereqs (see ../../docs/DEPLOYMENT.md): a Ceph RGW user+bucket, a 1Password item
# `dejaveph-ceph-rgw`, and an opnix service-account token at /etc/opnix-token.
{
  description = "Dejaveph gateway — xetd on Ceph RGW with 1Password (opnix) secrets";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    dejaveph.url = "github:SEBK4C/Dejaveph";
    opnix.url = "github:brizzbuzz/opnix";
  };

  outputs = { self, nixpkgs, dejaveph, opnix, ... }:
    let system = "x86_64-linux";
    in {
      nixosConfigurations.gateway = nixpkgs.lib.nixosSystem {
        inherit system;
        specialArgs = { inherit dejaveph; };
        modules = [
          dejaveph.nixosModules.default
          opnix.nixosModules.default
          ({ pkgs, dejaveph, ... }: {
            networking.hostName = "dejaveph"; # -> dejaveph.home.arpa

            # 1Password -> tmpfs dotenv (verify opnix option names against your pinned opnix).
            services.onepassword-secrets = {
              enable = true;
              tokenFile = "/etc/opnix-token";
              secrets.dejaveph-rgw = {
                path = "/run/secrets/dejaveph-ceph-rgw.env";
                mode = "0400";
                format = "dotenv";
                reference = ''
                  AWS_ACCESS_KEY_ID=op://Infrastructure/dejaveph-ceph-rgw/access_key_id
                  AWS_SECRET_ACCESS_KEY=op://Infrastructure/dejaveph-ceph-rgw/secret_access_key
                '';
              };
            };

            services.xetd = {
              enable = true;
              package = dejaveph.packages.${pkgs.system}.xetd-s3;
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
            systemd.services.xetd = {
              after = [ "onepassword-secrets.service" ];
              wants = [ "onepassword-secrets.service" ];
            };

            # TODO: front xetd (and RGW) with a TLS reverse proxy — xetd speaks plain HTTP and
            # bearer tokens travel in cleartext. e.g. services.nginx or services.caddy.

            system.stateVersion = "24.11";
          })
        ];
      };
    };
}
