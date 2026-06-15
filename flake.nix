{
  description = "Dejaveph — self-hosted Xet CAS server + reconstructing FUSE VFS";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    (flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Pin the toolchain: the vendored SEBK4C/xet-core fork is edition 2024, so it needs
        # rustc >= 1.85. Pinning makes the build reproducible on any machine with Nix.
        rustToolchain = pkgs.rust-bin.stable."1.85.0".default;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # `fuse` provides the setuid `fusermount` the xetfs mount shells out to (we build the
        # `fuser` crate with default-features=false, so no libfuse linkage is required).
        runtimeDeps = [ pkgs.fuse ];
      in
      {
        # `nix develop` — everything needed to `cargo build`/`cargo test` by hand.
        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain pkgs.fuse pkgs.pkg-config pkgs.cacert pkgs.git ];
          shellHook = ''
            echo "Dejaveph dev shell · $(rustc --version)"
            echo "  cargo test --workspace -- --test-threads=1   # FUSE tests need /dev/fuse + fusermount"
          '';
        };

        # `nix build .#xetd` -> ./result/bin/xetd   ·   `nix run .#xetd -- --help`
        #
        # The vendored fork lives in a git submodule, so build with submodules fetched:
        #   nix build "git+https://github.com/SEBK4C/Dejaveph?submodules=1#xetd"
        # (or `nix build .#xetd` from a checkout that already has `git submodule update --init`).
        packages.xetd = rustPlatform.buildRustPackage {
          pname = "xetd";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock; # fork crates are in-tree path deps — no extra hashes
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = runtimeDeps;
          cargoBuildFlags = [ "-p" "xetd" ];
          # The FUSE/round-trip integration tests need /dev/fuse, unavailable in the sandboxed
          # builder; run them in `nix develop` instead.
          doCheck = false;
          meta = with pkgs.lib; {
            description = "Self-hosted Xet CAS server + reconstructing FUSE VFS";
            license = licenses.asl20;
            mainProgram = "xetd";
          };
        };
        # `nix build .#xetd-s3` — the Ceph/S3 backend build (`--features s3`, pulls aws-sdk-s3).
        # The NixOS module's S3 backend requires THIS package, not the lean default.
        packages.xetd-s3 = self.packages.${system}.xetd.overrideAttrs (old: {
          pname = "xetd-s3";
          cargoBuildFlags = [ "-p" "xetd" "--features" "s3" ];
          # aws-sdk-s3's default TLS stack (aws-lc-rs / rustls) builds native code.
          nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.cmake pkgs.perl ];
          buildInputs = (old.buildInputs or [ ]) ++ [ pkgs.openssl ];
        });

        # `nix build .#xetfs` -> ./result/bin/xetfs   (the client-side mount CLI)
        packages.xetfs = self.packages.${system}.xetd.overrideAttrs (old: {
          pname = "xetfs";
          cargoBuildFlags = [ "-p" "xetfs" ];
          meta = (old.meta or { }) // { mainProgram = "xetfs"; };
        });

        packages.default = self.packages.${system}.xetd;

        # `nix flake check` builds the server variants + the mount CLI.
        checks.xetd = self.packages.${system}.xetd;
        checks.xetd-s3 = self.packages.${system}.xetd-s3;
        checks.xetfs = self.packages.${system}.xetfs;
      })) // {
        # System-independent NixOS modules (see ./nixos/*.nix and docs/DEPLOYMENT.md).
        # `default` = server; compose `xetd` + `xetfs` for a full server+client deployment.
        # Import alongside opnix for 1Password-backed RGW creds and mount tokens.
        nixosModules.default = import ./nixos/module.nix;
        nixosModules.xetd = import ./nixos/module.nix;
        nixosModules.xetfs = import ./nixos/xetfs.nix;
      };
}
