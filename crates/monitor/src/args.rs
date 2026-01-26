use clap::Args;

#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct FullLinkMonitorArgs {
    /// Enable full link monitor functionality
    #[arg(
        long = "xlayer.full-link-monitor.enable",
        help = "Enable full link monitor functionality (disabled by default)",
        default_value = "false"
    )]
    pub enable: bool,

    /// Output path for full link monitor logs
    #[arg(
        long = "xlayer.full-link-monitor.output-path",
        help = "Output path for full link monitor logs",
        default_value = "/data/logs/trace.log"
    )]
    pub output_path: String,
}

impl FullLinkMonitorArgs {
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}
