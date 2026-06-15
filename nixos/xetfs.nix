# NixOS module for client-side xetfs mounts — the other half of plug-and-play deployment.
#
# Each `services.xetfs.mounts.<name>` becomes a foreground systemd service that mounts a xetd
# volume as a reconstructing FUSE filesystem. For a tokens-mode server, the per-mount
# `tokenFile` supplies XETD_TOKEN as an EnvironmentFile — point it at an opnix-rendered secret
# to source the bearer token from 1Password (same pattern as the RGW creds in module.nix).
{ config, lib, pkgs, ... }:

let
  cfg = config.services.xetfs;
  inherit (lib) mkOption mkIf types mapAttrs' nameValuePair escapeShellArgs optional optionals;

  mountUnit = name: m: nameValuePair "xetfs-${name}" {
    description = "xetfs mount: ${m.volume} -> ${m.mountpoint}";
    wantedBy = [ "multi-user.target" ];
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    serviceConfig = {
      Type = "simple";
      ExecStartPre = "${pkgs.coreutils}/bin/mkdir -p ${m.mountpoint}";
      ExecStart = escapeShellArgs (
        [ "${cfg.package}/bin/xetfs" "--server" m.server "--volume" m.volume ]
        ++ optionals m.readWrite [ "--rw" ]
        ++ [ (toString m.mountpoint) ]
      );
      ExecStop = "${pkgs.fuse}/bin/fusermount -u ${m.mountpoint}";
      # Tokens-mode bearer (XETD_TOKEN) — render this from 1Password via opnix for parity with
      # the server's RGW creds; agnostic to the secret manager (any EnvironmentFile works).
      EnvironmentFile = optional (m.tokenFile != null) m.tokenFile;
      Restart = "on-failure";
      RestartSec = "3s";
      # FUSE needs /dev/fuse + the setuid fusermount, so we do NOT use DynamicUser/PrivateDevices.
    };
  };
in
{
  options.services.xetfs = {
    package = mkOption {
      type = types.package;
      description = "The xetfs package providing /bin/xetfs (the flake's packages.xetfs).";
    };

    mounts = mkOption {
      default = { };
      description = "Named xetfs mounts; each becomes an `xetfs-<name>.service`.";
      example = lib.literalExpression ''
        {
          models = {
            server = "http://dejaveph.home.arpa:9777";
            volume = "models";
            mountpoint = "/mnt/models";
          };
          scratch = {
            server = "http://dejaveph.home.arpa:9777";
            volume = "scratch";
            mountpoint = "/mnt/scratch";
            readWrite = true;
            tokenFile = "/run/secrets/dejaveph-xetd-token.env"; # XETD_TOKEN=... (from 1Password)
          };
        }
      '';
      type = types.attrsOf (types.submodule {
        options = {
          server = mkOption {
            type = types.str;
            example = "http://dejaveph.home.arpa:9777";
            description = "xetd base URL.";
          };
          volume = mkOption { type = types.str; description = "Volume name in the VFS catalog."; };
          mountpoint = mkOption { type = types.path; description = "Directory to mount at (created if absent)."; };
          readWrite = mkOption {
            type = types.bool;
            default = false;
            description = "Mount read-write (write-back on close). Default read-only.";
          };
          tokenFile = mkOption {
            type = types.nullOr types.path;
            default = null;
            example = "/run/secrets/dejaveph-xetd-token.env";
            description = "EnvironmentFile exporting XETD_TOKEN for a tokens-mode server.";
          };
        };
      });
    };
  };

  config = mkIf (cfg.mounts != { }) {
    environment.systemPackages = [ pkgs.fuse ];
    systemd.services = mapAttrs' mountUnit cfg.mounts;
  };
}
