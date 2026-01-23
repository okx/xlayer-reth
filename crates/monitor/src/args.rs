use clap::Args;

#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub struct FullLinkMonitorArgs {
    /// Enable transaction trace functionality
    #[arg(
        long = "tx-trace.enable",
        help = "Enable transaction trace functionality (disabled by default)",
        default_value = "false"
    )]
    pub tx_trace_enable: bool,

    /// Output path for transaction trace logs
    #[arg(
        long = "tx-trace.output-path",
        help = "Output path for transaction trace logs",
        default_value = "/data/logs/trace.log"
    )]
    pub tx_trace_output_path: String,
}

impl FullLinkMonitorArgs {
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}
