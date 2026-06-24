# NixOS module for the Dejaveph Xet CAS server (`xetd`) + reconstructing FUSE mount (`xetfs`).
#
# Goals:
#   * plug-and-play `services.xetd.enable = true;` with sane, hardened systemd defaults;
#   * Ceph/RGW (or any S3) backend wired in one block;
#   * **secrets never in the Nix store** — the RGW access/secret keys are delivered at runtime
#     via systemd credentials, populated from 1Password (opnix) or any EnvironmentFile.
#
# See ./example.nix for a complete host using opnix, and ../docs/DEPLOYMENT.md for the Ceph
# bootstrap + 1Password vault/item/host-name conventions.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.xetd;
  inherit (lib) mkEnableOption mkOption mkIf types optionals optionalString;
in
{
  options.services.xetd = {
    enable = mkEnableOption "the Dejaveph Xet CAS server (xetd)";

    package = mkOption {
      type = types.package;
      description = ''
        The xetd package. For the S3/Ceph backend this MUST be built with the `s3` feature
        (the flake exposes `packages.xetd-s3`); the default `packages.xetd` is local-fs only.
      '';
    };

    user = mkOption { type = types.str; default = "xetd"; description = "Service user."; };
    group = mkOption { type = types.str; default = "xetd"; description = "Service group."; };

    listen = mkOption {
      type = types.str;
      default = "127.0.0.1:9777";
      example = "0.0.0.0:9777";
      description = ''
        `host:port` to bind. Keep loopback unless a TLS-terminating reverse proxy fronts it —
        xetd speaks plain HTTP and bearer tokens travel in cleartext.
      '';
    };

    dataDir = mkOption {
      type = types.path;
      default = "/var/lib/xetd";
      description = "State dir (index DB + local-fs blobs). Created with 0750 ownership.";
    };

    durability = mkOption {
      type = types.enum [ "close" "fsync" ];
      default = "fsync";
      description = "Blob write durability. `fsync` is the safe default for a real deployment.";
    };

    auth = mkOption {
      type = types.enum [ "loopback" "tokens" ];
      default = "loopback";
      description = ''
        `loopback` trusts every local caller (single-user). `tokens` enforces bearer scopes
        (POST=write, GET=read).
      '';
    };

    backend = mkOption {
      type = types.enum [ "local-fs" "s3" ];
      default = "local-fs";
      description = "Blob backend. `s3` targets Ceph RGW (or any S3) — see `s3.*` below.";
    };

    s3 = {
      endpoint = mkOption {
        type = types.str;
        default = "";
        example = "https://rgw.ceph.home.arpa";
        description = "Ceph RGW / S3 endpoint URL.";
      };
      bucket = mkOption {
        type = types.str;
        default = "dejaveph-xorbs";
        description = "Bucket holding the immutable, content-addressed xorbs.";
      };
      pathStyle = mkOption {
        type = types.bool;
        default = true;
        description = "Path-style addressing — the safe default for RGW.";
      };
      credentialsFile = mkOption {
        type = types.nullOr types.path;
        default = null;
        example = "/run/secrets/dejaveph-ceph-rgw.env";
        description = ''
          Path to an EnvironmentFile exporting AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY for
          the RGW user. Delivered at runtime (NOT in the Nix store). With opnix, point this at
          the rendered secret file (see ./example.nix). The file is loaded via systemd
          LoadCredential so it never lands in the unit's environment in `ps`/journal.
        '';
      };
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open the `listen` port in the firewall (only if binding a non-loopback addr).";
    };

    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Extra CLI flags passed verbatim to xetd.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.backend != "s3" || cfg.s3.endpoint != "";
        message = "services.xetd: backend = \"s3\" requires services.xetd.s3.endpoint.";
      }
      {
        assertion = cfg.backend != "s3" || cfg.s3.credentialsFile != null;
        message = ''
          services.xetd: backend = "s3" requires services.xetd.s3.credentialsFile
          (an EnvironmentFile with AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY — see DEPLOYMENT.md).
        '';
      }
    ];

    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      home = cfg.dataDir;
    };
    users.groups.${cfg.group} = { };

    networking.firewall.allowedTCPPorts =
      optionals cfg.openFirewall [ (lib.toInt (lib.last (lib.splitString ":" cfg.listen))) ];

    systemd.services.xetd = {
      description = "Dejaveph Xet CAS server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = {
        User = cfg.user;
        Group = cfg.group;
        StateDirectory = "xetd";
        StateDirectoryMode = "0750";

        # Secret delivery: load the RGW creds as a systemd credential (tmpfs, 0400, owned by the
        # service). The ExecStart wrapper sources it just before exec so it reaches xetd's env
        # without ever being written to the store or shown in `systemctl show`.
        LoadCredential =
          optionals (cfg.backend == "s3" && cfg.s3.credentialsFile != null)
            [ "rgw:${cfg.s3.credentialsFile}" ];

        ExecStart =
          let
            base = [
              "${cfg.package}/bin/xetd"
              "--listen" cfg.listen
              "--data-dir" cfg.dataDir
              "--db" "${cfg.dataDir}/index.sqlite"
              "--durability" cfg.durability
              "--auth" cfg.auth
              "--backend" cfg.backend
            ];
            localFs = optionals (cfg.backend == "local-fs") [ "--blob-root" "${cfg.dataDir}/blobs" ];
            s3 = optionals (cfg.backend == "s3") ([
              "--s3-endpoint" cfg.s3.endpoint
              "--s3-bucket" cfg.s3.bucket
            ] ++ optionals cfg.s3.pathStyle [ "--s3-path-style" ]);
            argv = lib.escapeShellArgs (base ++ localFs ++ s3 ++ cfg.extraArgs);
          in
          pkgs.writeShellScript "xetd-start" ''
            set -euo pipefail
            ${optionalString (cfg.backend == "s3" && cfg.s3.credentialsFile != null) ''
              # Source the RGW creds from the systemd credential (tmpfs, not the store).
              set -a; . "''${CREDENTIALS_DIRECTORY}/rgw"; set +a
            ''}
            exec ${argv}
          '';

        Restart = "on-failure";
        RestartSec = "2s";

        # Hardening — xetd needs only its StateDirectory and (for the FUSE consumer) /dev/fuse.
        DynamicUser = false; # we manage a stable uid so blob ownership survives redeploys
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallFilter = [ "@system-service" "~@privileged" "~@resources" ];
        SystemCallArchitectures = "native";
        UMask = "0077";
      };
    };
  };
}
