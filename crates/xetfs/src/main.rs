//! `xetfs` mount CLI — mount a `xetd` volume as a reconstructing FUSE filesystem.
//!
//! Foreground mount: the process stays alive while mounted and unmounts on SIGINT/SIGTERM (the
//! kernel tears the session down when this exits). That makes it a clean systemd unit — see
//! `nixos/xetfs.nix` for a `services.xetfs.mounts.<name>` wrapper.
//!
//!   xetfs --server http://dejaveph.home.arpa:9777 --volume models /mnt/models        # read-only
//!   xetfs --server http://dejaveph.home.arpa:9777 --volume scratch --rw /mnt/scratch # writable

use std::path::PathBuf;

use clap::Parser;
use fuser::MountOption;

#[derive(Parser, Debug)]
#[command(name = "xetfs", about = "Mount a Dejaveph/Xet volume as a FUSE filesystem")]
struct Args {
    /// xetd base URL, e.g. http://dejaveph.home.arpa:9777
    #[arg(long)]
    server: String,
    /// Volume name in the VFS catalog (§9.1).
    #[arg(long)]
    volume: String,
    /// Mount read-write (write-back on close). Default: read-only.
    #[arg(long, default_value_t = false)]
    rw: bool,
    /// Existing directory to mount at.
    mountpoint: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    anyhow::ensure!(
        args.mountpoint.is_dir(),
        "mountpoint {} does not exist or is not a directory",
        args.mountpoint.display()
    );

    let fs = xetfs::Xetfs::connect(&args.server, &args.volume, args.rw)
        .map_err(|e| anyhow::anyhow!("connecting to xetd at {}: {e}", args.server))?;

    let mut opts = vec![MountOption::FSName("xetfs".into()), MountOption::DefaultPermissions];
    opts.push(if args.rw { MountOption::RW } else { MountOption::RO });

    eprintln!(
        "xetfs: mounting volume '{}' from {} at {} ({})",
        args.volume,
        args.server,
        args.mountpoint.display(),
        if args.rw { "rw" } else { "ro" }
    );

    // Blocks until unmounted (fusermount -u, or signal-driven teardown).
    fuser::mount2(fs, &args.mountpoint, &opts)?;
    Ok(())
}
