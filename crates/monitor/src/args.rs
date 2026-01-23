use clap::Args;

#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct FullLinkMonitorArgs {
    /// Enable full link monitor functionality
    #[arg(
        long = "xlayer.full-link-monitor",
        help = "Enable full link monitor functionality (disabled by default)",
        default_value = "false"
    )]
    pub enable: bool,
}

impl FullLinkMonitorArgs {
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}
