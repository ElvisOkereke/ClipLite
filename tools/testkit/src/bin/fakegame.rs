//! `fakegame` — D3D11 load generator for ClipLite hardware-in-the-loop tests.
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cliplite_testkit::fakegame::Args::parse();
    #[cfg(windows)]
    {
        cliplite_testkit::fakegame::render::run(args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        anyhow::bail!("fakegame requires Windows (D3D11)")
    }
}
