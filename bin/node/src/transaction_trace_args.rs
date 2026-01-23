use clap::Args;
use std::path::PathBuf;

/// Transaction trace configuration arguments
#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
#[command(next_help_heading = "Transaction Trace")]
pub struct TransactionTraceArgs {
    /// Enable transaction tracing
    #[arg(
        long = "tx-trace.enable",
        help = "Enable transaction and block lifecycle tracing",
        default_value = "false"
    )]
    pub enable: bool,

    /// Output path for trace log file
    #[arg(
        long = "tx-trace.output-path",
        value_name = "PATH",
        help = "Path to the trace log output file (CSV format). If not specified, tracing is disabled.",
        value_hint = clap::ValueHint::FilePath
    )]
    pub output_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args, Parser};

    /// A helper type to parse Args more easily
    #[derive(Parser)]
    struct CommandParser<T: Args> {
        #[command(flatten)]
        args: T,
    }

    #[test]
    fn test_transaction_trace_args_default() {
        let args = CommandParser::<TransactionTraceArgs>::parse_from(["reth"]).args;
        assert!(!args.enable);
        assert!(args.output_path.is_none());
    }

    #[test]
    fn test_transaction_trace_args_enable() {
        let args = CommandParser::<TransactionTraceArgs>::parse_from([
            "reth",
            "--tx-trace.enable",
        ])
        .args;
        assert!(args.enable);
        assert!(args.output_path.is_none());
    }

    #[test]
    fn test_transaction_trace_args_with_path() {
        let args = CommandParser::<TransactionTraceArgs>::parse_from([
            "reth",
            "--tx-trace.enable",
            "--tx-trace.output-path",
            "/data/logs/trace.log",
        ])
        .args;
        assert!(args.enable);
        assert_eq!(
            args.output_path,
            Some(PathBuf::from("/data/logs/trace.log"))
        );
    }
}

