# Dejaveph CLIENT host — mounts xetd volumes as reconstructing FUSE filesystems.
#
#   nix flake init -t github:SEBK4C/Dejaveph#client
#
# Edit: hostName, the server URL, and the volume/mountpoint list. A read-write mount against a
# tokens-mode server needs XETD_TOKEN — render it from 1Password (item `dejaveph-xetd-tokens`)
# to the tokenFile, same opnix pattern as the gateway's RGW creds.
{
  description = "Dejaveph client — xetfs mounts";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    dejaveph.url = "github:SEBK4C/Dejaveph";
    opnix.url = "github:brizzbuzz/opnix";
  };

  outputs = { self, nixpkgs, dejaveph, opnix, ... }:
    let system = "x86_64-linux";
    in {
      nixosConfigurations.client = nixpkgs.lib.nixosSystem {
        inherit system;
        specialArgs = { inherit dejaveph; };
        modules = [
          dejaveph.nixosModules.xetfs
          opnix.nixosModules.default
          ({ pkgs, dejaveph, ... }: {
            networking.hostName = "dejaveph-mnt";

            # Bearer token for a tokens-mode server (only needed for read-write or non-loopback).
            services.onepassword-secrets = {
              enable = true;
              tokenFile = "/etc/opnix-token";
              secrets.dejaveph-token = {
                path = "/run/secrets/dejaveph-xetd-token.env";
                mode = "0400";
                format = "dotenv";
                reference = "XETD_TOKEN=op://Infrastructure/dejaveph-xetd-tokens/write_token";
              };
            };

            services.xetfs = {
              package = dejaveph.packages.${pkgs.system}.xetfs;
              mounts = {
                models = {
                  server = "http://dejaveph.home.arpa:9777";
                  volume = "models";
                  mountpoint = "/mnt/models"; # read-only
                };
                scratch = {
                  server = "http://dejaveph.home.arpa:9777";
                  volume = "scratch";
                  mountpoint = "/mnt/scratch";
                  readWrite = true;
                  tokenFile = "/run/secrets/dejaveph-xetd-token.env";
                };
              };
            };

            system.stateVersion = "24.11";
          })
        ];
      };
    };
}
