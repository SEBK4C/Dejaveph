# Dejaveph DEMO — one box, zero secrets, zero Ceph. xetd on the local-fs backend plus a local
# xetfs mount. The fastest way to see it work; collapse server + client onto a single host.
#
#   nix flake init -t github:SEBK4C/Dejaveph#demo
#   nixos-rebuild switch --flake .#demo
#
# When you outgrow one disk, switch `backend` to "s3" (see the `gateway` template) — that is the
# only change needed to move to Ceph.
{
  description = "Dejaveph demo — single-box xetd (local-fs) + a local xetfs mount, no secrets";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    dejaveph.url = "github:SEBK4C/Dejaveph";
  };

  outputs = { self, nixpkgs, dejaveph, ... }:
    let system = "x86_64-linux";
    in {
      nixosConfigurations.demo = nixpkgs.lib.nixosSystem {
        inherit system;
        specialArgs = { inherit dejaveph; };
        modules = [
          dejaveph.nixosModules.default
          dejaveph.nixosModules.xetfs
          ({ pkgs, dejaveph, ... }: {
            networking.hostName = "dejaveph-demo";

            # Server: local-fs backend, loopback auth — no bucket, no tokens, no 1Password.
            services.xetd = {
              enable = true;
              package = dejaveph.packages.${pkgs.system}.xetd; # lean (non-s3) build
              listen = "127.0.0.1:9777";
              backend = "local-fs";
              auth = "loopback";
              durability = "fsync";
            };

            # Client: mount a volume locally. Loopback server ⇒ no token needed.
            services.xetfs = {
              package = dejaveph.packages.${pkgs.system}.xetfs;
              mounts.demo = {
                server = "http://127.0.0.1:9777";
                volume = "demo";
                mountpoint = "/mnt/dejaveph";
                readWrite = true;
              };
            };
            systemd.services."xetfs-demo" = {
              after = [ "xetd.service" ];
              wants = [ "xetd.service" ];
            };

            system.stateVersion = "24.11";
          })
        ];
      };
    };
}
