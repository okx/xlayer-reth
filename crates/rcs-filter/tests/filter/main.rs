//! Integration-test binary for the series' screening tests of `rcs-filter`.
mod callstack {
    #[allow(unused_imports)]
    use alloy_primitives::{Address, B256};
    #[allow(unused_imports)]
    use rcs_filter::callstack::*;
    mod tests;
}
mod callstack_tests;
mod client {
    mod wire_tests {
        #[allow(unused_imports)]
        use rcs_filter::client::*;
        mod response_root_tests;
    }
}
mod config {
    mod tests {
        #[allow(unused_imports)]
        use rcs_filter::config::*;
        mod callstack_config_tests;
    }
}
mod raw_wire_tests;
mod rules {
    mod validation {
        #[allow(unused_imports)]
        use rcs_filter::rules::*;
        mod strict_schema_tests;
        mod tests {
            #[allow(unused_imports)]
            use super::*;
            /// Fixture copied verbatim from `validation.rs`'s pre-existing `tests` module.
            fn parse(json: &str) -> RawRule {
                serde_json::from_str(json).expect("valid rule json")
            }
            mod selector_family_tests;
        }
    }
}
mod timed_out_allow_recheck_api_tests;
mod timed_out_allow_recheck_tests;
mod upstream_rules_tests;
