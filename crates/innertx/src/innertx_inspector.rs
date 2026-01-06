use alloy_primitives::{hex, Address, Bytes, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use reth_revm::{
    interpreter::{
        interpreter::EthInterpreter, CallInput, CallInputs, CallOutcome, CreateInputs,
        CreateOutcome, InstructionResult,
    },
    Inspector,
};
use revm_context_interface::{ContextTr, LocalContextTr};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serialize Bytes as empty string if empty, otherwise as hex string
fn serialize_bytes_legacy<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if bytes.is_empty() {
        serializer.serialize_str("")
    } else {
        serializer.serialize_str(&format!("0x{}", hex::encode(bytes)))
    }
}

/// Deserialize Bytes from empty string or hex string
fn deserialize_bytes_legacy<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        Ok(Bytes::default())
    } else {
        let hex_str = s.strip_prefix("0x").unwrap_or(&s);
        hex::decode(hex_str).map(Bytes::from).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Default, RlpEncodable, RlpDecodable, Serialize, Deserialize)]
pub struct InternalTransaction {
    dept: u64,
    internal_index: u64,
    call_type: String,
    name: String,
    trace_address: String,
    code_address: String,
    from: String,
    to: String,
    #[serde(
        serialize_with = "serialize_bytes_legacy",
        deserialize_with = "deserialize_bytes_legacy"
    )]
    input: Bytes,
    #[serde(
        serialize_with = "serialize_bytes_legacy",
        deserialize_with = "deserialize_bytes_legacy"
    )]
    output: Bytes,
    is_error: bool,
    gas: u64,
    gas_used: u64,
    value: String,
    value_wei: String,
    call_value_wei: String,
    error: String,
}

impl InternalTransaction {
    pub fn set_transaction_gas(&mut self, gas_limit: u64, gas_used: u64) {
        self.gas = gas_limit;
        self.gas_used = gas_used;
    }
}

#[derive(Debug, Clone)]
pub struct TraceCollector {
    all_traces: Vec<Vec<InternalTransaction>>,
    traces: Vec<InternalTransaction>,
    // depth
    current_path: Vec<usize>,
    // internal_index
    last_depth: usize,
    sibling_count: Vec<usize>,
    trace_stack: Vec<usize>,
}

impl Default for TraceCollector {
    fn default() -> Self {
        Self {
            all_traces: Vec::<Vec<InternalTransaction>>::default(),
            traces: Vec::<InternalTransaction>::default(),
            current_path: Vec::<usize>::default(),
            last_depth: 0,
            sibling_count: vec![0],
            trace_stack: Vec::<usize>::default(),
        }
    }
}

impl TraceCollector {
    fn format_error(result: &InstructionResult) -> String {
        match result {
            InstructionResult::Revert => "execution reverted".to_string(),
            InstructionResult::CallTooDeep => "max call depth exceeded".to_string(),
            InstructionResult::OutOfGas => "out of gas".to_string(),
            InstructionResult::NonceOverflow => "nonce uint64 overflow".to_string(),
            InstructionResult::InvalidJump => "invalid jump destination".to_string(),
            InstructionResult::CreateCollision => "contract address collision".to_string(),
            InstructionResult::OutOfFunds => "insufficient balance for transfer".to_string(),
            InstructionResult::CreateInitCodeSizeLimit => "max initcode size exceeded".to_string(),
            InstructionResult::OpcodeNotFound => "invalid opcode".to_string(),
            InstructionResult::ReentrancySentryOOG => {
                "not enough gas for reentrancy sentry".to_string()
            }
            InstructionResult::StackUnderflow => "stack underflow".to_string(),
            InstructionResult::StackOverflow => "stack overflow".to_string(),
            InstructionResult::CreateInitCodeStartingEF00 => {
                "CREATE/CREATE2 starts with 0xEF00".to_string()
            }
            InstructionResult::InvalidEOFInitCode => {
                "invalid EVM Object Format (EOF) init code".to_string()
            }
            InstructionResult::InvalidExtDelegateCallTarget => {
                "extDelegateCall calling a non EOF contract".to_string()
            }
            InstructionResult::MemoryOOG => {
                "out of gas error encountered during memory expansion".to_string()
            }
            InstructionResult::MemoryLimitOOG => {
                "the memory limit of the EVM has been exceeded".to_string()
            }
            InstructionResult::PrecompileOOG => {
                "out of gas error encountered during the execution of a precompiled contract"
                    .to_string()
            }
            InstructionResult::InvalidOperandOOG => {
                "out of gas error encountered while calling an invalid operand".to_string()
            }
            InstructionResult::CallNotAllowedInsideStatic => {
                "invalid CALL with value transfer in static context".to_string()
            }
            InstructionResult::StateChangeDuringStaticCall => {
                "invalid state modification in static call".to_string()
            }
            InstructionResult::InvalidFEOpcode => {
                "an undefined bytecode value encountered during execution".to_string()
            }
            InstructionResult::NotActivated => {
                "the feature or opcode is not activated in this version of the EVM".to_string()
            }
            InstructionResult::OutOfOffset => "invalid memory or storage offset".to_string(),
            InstructionResult::OverflowPayment => "payment amount overflow".to_string(),
            InstructionResult::PrecompileError => {
                "error in precompiled contract execution".to_string()
            }
            InstructionResult::CreateContractSizeLimit => {
                "exceeded contract size limit during creation".to_string()
            }
            InstructionResult::CreateContractStartingWithEF => {
                "created contract starts with invalid bytes 0xEF".to_string()
            }
            InstructionResult::FatalExternalError => "fatal external error".to_string(),
            _ => format!("{result:?}"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn init_op(
        &mut self,
        call_type: String,
        from: String,
        to: String,
        input: Bytes,
        value_wei: String,
        gas_limit: u64,
        code_address: String,
    ) {
        let mut txn = InternalTransaction {
            call_type,
            from: from.clone(),
            input,
            gas: gas_limit,
            value_wei: if value_wei.is_empty() { "0" } else { &value_wei }.to_string(),
            call_value_wei: match value_wei.parse::<u128>() {
                Ok(value) => format!("0x{value:x}"),
                _ => String::from("0x0"),
            },
            to: to.clone(),
            ..Default::default()
        };

        match txn.call_type.as_str() {
            "delegatecall" => {
                txn.from = to;
                txn.to = code_address;
                txn.trace_address = txn.from.clone();
            }
            "callcode" => {
                txn.code_address = code_address;
            }
            _ => {}
        }

        self.traces.push(txn);
    }

    fn before_op(&mut self) {
        let depth = self.current_path.len();

        if depth > self.last_depth {
            self.sibling_count.push(0);
        } else if depth < self.last_depth {
            self.sibling_count.truncate(depth + 1);
        }
        self.last_depth = depth;

        let internal_index = self.sibling_count[depth];
        let trace_index = self.traces.len() - 1;
        self.trace_stack.push(trace_index);

        let txn = &mut self.traces[trace_index];
        txn.dept = depth as u64;
        txn.internal_index = internal_index as u64;

        self.sibling_count[depth] += 1;

        self.current_path.push(internal_index);
        if self.current_path.len() > 1 {
            let suffix =
                self.current_path[1..].iter().map(|s| s.to_string()).collect::<Vec<_>>().join("_");

            txn.name.reserve(1 + suffix.len());
            txn.name.push('_');
            txn.name.push_str(&suffix);
        }

        txn.name = txn.call_type.clone() + &txn.name;
    }

    fn after_op(&mut self) {
        self.current_path.pop();
        if self.trace_stack.is_empty() {
            self.all_traces.push(self.traces.clone());
            self.reset();
        }
    }

    pub fn get(&mut self) -> Vec<Vec<InternalTransaction>> {
        self.all_traces.clone()
    }

    pub fn reset(&mut self) {
        self.traces.clear();
        self.current_path.clear();
        self.last_depth = 0;
        self.sibling_count = vec![0];
        self.trace_stack.clear();
    }
}

impl<CTX> Inspector<CTX, EthInterpreter> for TraceCollector
where
    CTX: ContextTr,
{
    fn call(&mut self, ctx: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        let call_type = match inputs.scheme {
            reth_revm::interpreter::CallScheme::Call => "call",
            reth_revm::interpreter::CallScheme::CallCode => "callcode",
            reth_revm::interpreter::CallScheme::DelegateCall => "delegatecall",
            reth_revm::interpreter::CallScheme::StaticCall => "staticcall",
        }
        .to_string();

        let call_input = match &inputs.input {
            CallInput::SharedBuffer(range) => ctx
                .local()
                .shared_memory_buffer_slice(range.clone())
                .map(|s| Bytes::from(s.to_vec()))
                .unwrap_or_default(),
            CallInput::Bytes(b) => b.clone(),
        };

        self.init_op(
            call_type,
            inputs.caller.to_string(),
            inputs.target_address.to_string(),
            call_input,
            inputs.value.get().to_string(),
            inputs.gas_limit,
            inputs.bytecode_address.to_string(),
        );

        self.before_op();

        None
    }

    fn call_end(&mut self, _ctx: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        let trace_index = self.trace_stack.pop().unwrap_or_default();
        let (_, after) = self.traces.split_at_mut(trace_index);

        if let Some((txn, remainder)) = after.split_first_mut() {
            txn.gas_used = outcome.result.gas.spent();
            txn.output = outcome.result.output.clone();
            txn.is_error = !outcome.result.is_ok();
            txn.error = if txn.is_error {
                Self::format_error(&outcome.result.result)
            } else {
                String::new()
            };
            if txn.is_error {
                for within in remainder {
                    within.is_error = txn.is_error;
                }
            }
        }

        self.after_op();
    }

    fn create(&mut self, _ctx: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        let call_type = match inputs.scheme {
            reth_revm::interpreter::CreateScheme::Create => "create".to_string(),
            reth_revm::interpreter::CreateScheme::Create2 { salt: _ } => "create2".to_string(), /* code_address */
            reth_revm::interpreter::CreateScheme::Custom { address: _ } => "custom".to_string(),
        };

        self.init_op(
            call_type,
            inputs.caller.to_string(),
            "".to_string(),
            inputs.init_code.clone(),
            inputs.value.to_string(),
            inputs.gas_limit,
            "".to_string(),
        );

        self.before_op();

        None
    }

    fn create_end(&mut self, _ctx: &mut CTX, _inputs: &CreateInputs, outcome: &mut CreateOutcome) {
        let trace_index = self.trace_stack.pop().unwrap_or_default();
        let (_, after) = self.traces.split_at_mut(trace_index);

        if let Some((txn, remainder)) = after.split_first_mut() {
            txn.to = outcome.address.unwrap_or_default().to_string();
            txn.gas_used = outcome.result.gas.spent();
            txn.output = outcome.result.output.clone();
            txn.is_error = !outcome.result.is_ok();
            txn.error = if txn.is_error {
                Self::format_error(&outcome.result.result)
            } else {
                String::new()
            };
            if txn.is_error {
                for within in remainder {
                    within.is_error = txn.is_error;
                }
            }
        }

        self.after_op();
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        self.init_op(
            "selfdestruct".to_string(),
            contract.to_string(),
            target.to_string(),
            Bytes::default(),
            value.to_string(),
            0,
            "".to_string(),
        );

        self.before_op();

        self.trace_stack.pop().unwrap_or_default();

        self.after_op();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod internal_transaction_tests {
        use super::*;

        #[test]
        fn test_serialize_empty_bytes_as_empty_string() {
            let txn = InternalTransaction {
                input: Bytes::default(),
                output: Bytes::default(),
                ..Default::default()
            };

            let json = serde_json::to_string(&txn).unwrap();
            assert!(json.contains(r#""input":"""#));
            assert!(json.contains(r#""output":"""#));
        }

        #[test]
        fn test_serialize_non_empty_bytes_as_hex() {
            let txn = InternalTransaction {
                input: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
                output: Bytes::from(vec![0x12, 0x34]),
                ..Default::default()
            };

            let json = serde_json::to_string(&txn).unwrap();
            assert!(json.contains(r#""input":"0xdeadbeef""#));
            assert!(json.contains(r#""output":"0x1234""#));
        }

        #[test]
        fn test_deserialize_empty_string_to_empty_bytes() {
            let json = r#"{"dept":0,"internal_index":0,"call_type":"","name":"","trace_address":"","code_address":"","from":"","to":"","input":"","output":"","is_error":false,"gas":0,"gas_used":0,"value":"","value_wei":"","call_value_wei":"","error":""}"#;
            let txn: InternalTransaction = serde_json::from_str(json).unwrap();
            assert!(txn.input.is_empty());
            assert!(txn.output.is_empty());
        }

        #[test]
        fn test_deserialize_hex_with_0x_prefix() {
            let json = r#"{"dept":0,"internal_index":0,"call_type":"","name":"","trace_address":"","code_address":"","from":"","to":"","input":"0xdeadbeef","output":"0x1234","is_error":false,"gas":0,"gas_used":0,"value":"","value_wei":"","call_value_wei":"","error":""}"#;
            let txn: InternalTransaction = serde_json::from_str(json).unwrap();
            assert_eq!(txn.input, Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));
            assert_eq!(txn.output, Bytes::from(vec![0x12, 0x34]));
        }

        #[test]
        fn test_deserialize_hex_without_0x_prefix() {
            let json = r#"{"dept":0,"internal_index":0,"call_type":"","name":"","trace_address":"","code_address":"","from":"","to":"","input":"deadbeef","output":"1234","is_error":false,"gas":0,"gas_used":0,"value":"","value_wei":"","call_value_wei":"","error":""}"#;
            let txn: InternalTransaction = serde_json::from_str(json).unwrap();
            assert_eq!(txn.input, Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));
            assert_eq!(txn.output, Bytes::from(vec![0x12, 0x34]));
        }

        #[test]
        fn test_deserialize_invalid_hex_returns_error() {
            let json = r#"{"dept":0,"internal_index":0,"call_type":"","name":"","trace_address":"","code_address":"","from":"","to":"","input":"0xZZZZ","output":"","is_error":false,"gas":0,"gas_used":0,"value":"","value_wei":"","call_value_wei":"","error":""}"#;
            let result: Result<InternalTransaction, _> = serde_json::from_str(json);
            assert!(result.is_err());
        }

        #[test]
        fn test_rlp_encode_decode_roundtrip() {
            use alloy_rlp::Decodable;

            let original = InternalTransaction {
                dept: 1,
                internal_index: 2,
                call_type: "call".to_string(),
                name: "test".to_string(),
                trace_address: "0x123".to_string(),
                code_address: "0x456".to_string(),
                from: "0xaaa".to_string(),
                to: "0xbbb".to_string(),
                input: Bytes::from(vec![1, 2, 3]),
                output: Bytes::from(vec![4, 5, 6]),
                is_error: false,
                gas: 100000,
                gas_used: 50000,
                value: "1000".to_string(),
                value_wei: "1000000000000000000".to_string(),
                call_value_wei: "0x0".to_string(),
                error: "".to_string(),
            };

            let encoded = alloy_rlp::encode(&original);
            let decoded = InternalTransaction::decode(&mut &encoded[..]).unwrap();

            assert_eq!(original.dept, decoded.dept);
            assert_eq!(original.internal_index, decoded.internal_index);
            assert_eq!(original.call_type, decoded.call_type);
            assert_eq!(original.input, decoded.input);
            assert_eq!(original.output, decoded.output);
        }

        #[test]
        fn test_set_transaction_gas_updates_both_fields() {
            let mut txn = InternalTransaction::default();
            txn.set_transaction_gas(100000, 50000);
            assert_eq!(txn.gas, 100000);
            assert_eq!(txn.gas_used, 50000);
        }

        #[test]
        fn test_set_transaction_gas_with_zero_values() {
            let mut txn = InternalTransaction::default();
            txn.set_transaction_gas(0, 0);
            assert_eq!(txn.gas, 0);
            assert_eq!(txn.gas_used, 0);
        }
    }

    mod trace_collector_tests {
        use super::*;

        #[test]
        fn test_format_error_revert() {
            let result = InstructionResult::Revert;
            assert_eq!(TraceCollector::format_error(&result), "execution reverted");
        }

        #[test]
        fn test_format_error_call_too_deep() {
            let result = InstructionResult::CallTooDeep;
            assert_eq!(TraceCollector::format_error(&result), "max call depth exceeded");
        }

        #[test]
        fn test_format_error_out_of_gas() {
            let result = InstructionResult::OutOfGas;
            assert_eq!(TraceCollector::format_error(&result), "out of gas");
        }

        #[test]
        fn test_format_error_out_of_funds() {
            let result = InstructionResult::OutOfFunds;
            assert_eq!(TraceCollector::format_error(&result), "insufficient balance for transfer");
        }

        #[test]
        fn test_format_error_invalid_jump() {
            let result = InstructionResult::InvalidJump;
            assert_eq!(TraceCollector::format_error(&result), "invalid jump destination");
        }

        #[test]
        fn test_format_error_stack_overflow() {
            let result = InstructionResult::StackOverflow;
            assert_eq!(TraceCollector::format_error(&result), "stack overflow");
        }

        #[test]
        fn test_format_error_stack_underflow() {
            let result = InstructionResult::StackUnderflow;
            assert_eq!(TraceCollector::format_error(&result), "stack underflow");
        }

        #[test]
        fn test_init_op_regular_call() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::from(vec![1, 2, 3]),
                "1000".to_string(),
                100000,
                "0xccc".to_string(),
            );

            assert_eq!(collector.traces.len(), 1);
            let txn = &collector.traces[0];
            assert_eq!(txn.call_type, "call");
            assert_eq!(txn.from, "0xaaa");
            assert_eq!(txn.to, "0xbbb");
            assert_eq!(txn.input, Bytes::from(vec![1, 2, 3]));
            assert_eq!(txn.gas, 100000);
            assert_eq!(txn.value_wei, "1000");
            assert_eq!(txn.call_value_wei, "0x3e8");
        }

        #[test]
        fn test_init_op_delegatecall_swaps_addresses() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "delegatecall".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "0xccc".to_string(),
            );

            let txn = &collector.traces[0];
            assert_eq!(txn.call_type, "delegatecall");
            assert_eq!(txn.from, "0xbbb");
            assert_eq!(txn.to, "0xccc");
            assert_eq!(txn.trace_address, "0xbbb");
        }

        #[test]
        fn test_init_op_callcode_sets_code_address() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "callcode".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "0xccc".to_string(),
            );

            let txn = &collector.traces[0];
            assert_eq!(txn.call_type, "callcode");
            assert_eq!(txn.code_address, "0xccc");
        }

        #[test]
        fn test_init_op_with_empty_value_defaults_to_zero() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "".to_string(),
                100000,
                "".to_string(),
            );

            let txn = &collector.traces[0];
            assert_eq!(txn.value_wei, "0");
            assert_eq!(txn.call_value_wei, "0x0");
        }

        #[test]
        fn test_init_op_value_wei_conversion_to_hex() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "255".to_string(),
                100000,
                "".to_string(),
            );

            let txn = &collector.traces[0];
            assert_eq!(txn.value_wei, "255");
            assert_eq!(txn.call_value_wei, "0xff");
        }

        #[test]
        fn test_init_op_invalid_value_wei_defaults_to_0x0() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "invalid".to_string(),
                100000,
                "".to_string(),
            );

            let txn = &collector.traces[0];
            assert_eq!(txn.call_value_wei, "0x0");
        }

        #[test]
        fn test_before_op_sets_depth_and_index() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );

            collector.before_op();

            let txn = &collector.traces[0];
            assert_eq!(txn.dept, 0);
            assert_eq!(txn.internal_index, 0);
            assert_eq!(txn.name, "call");
        }

        #[test]
        fn test_before_op_trace_naming_nested_depth() {
            let mut collector = TraceCollector::default();

            // First call at depth 0
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // Nested call at depth 1, index 0
            collector.init_op(
                "call".to_string(),
                "0xbbb".to_string(),
                "0xccc".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();
            // Return from first nested call
            collector.trace_stack.pop();
            collector.after_op();

            // Second nested call at depth 1, index 1
            collector.init_op(
                "staticcall".to_string(),
                "0xbbb".to_string(),
                "0xddd".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            assert_eq!(collector.traces[0].name, "call");
            assert_eq!(collector.traces[1].name, "call_0");
            assert_eq!(collector.traces[2].name, "staticcall_1");
        }

        #[test]
        fn test_before_op_sibling_count_increments() {
            let mut collector = TraceCollector::default();

            // Parent call
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // Three sibling calls at depth 1
            for i in 0..3 {
                collector.init_op(
                    "call".to_string(),
                    "0xbbb".to_string(),
                    "0xccc".to_string(),
                    Bytes::default(),
                    "0".to_string(),
                    50000,
                    "".to_string(),
                );
                collector.before_op();
                // Verify the sibling index
                assert_eq!(collector.traces[i + 1].internal_index, i as u64);
                // Pop and unwind to return to depth 1
                collector.trace_stack.pop();
                collector.after_op();
            }
        }

        #[test]
        fn test_after_op_pops_current_path() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            assert_eq!(collector.current_path.len(), 1);
            collector.after_op();
            assert_eq!(collector.current_path.len(), 0);
        }

        #[test]
        fn test_after_op_moves_to_all_traces_when_stack_empty() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            assert_eq!(collector.all_traces.len(), 0);
            // Pop the trace_stack to simulate call completion
            collector.trace_stack.pop();
            collector.after_op();
            assert_eq!(collector.all_traces.len(), 1);
            assert_eq!(collector.traces.len(), 0);
        }

        #[test]
        fn test_reset_clears_all_fields() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            collector.reset();

            assert!(collector.traces.is_empty());
            assert!(collector.current_path.is_empty());
            assert_eq!(collector.last_depth, 0);
            assert_eq!(collector.sibling_count, vec![0]);
            assert!(collector.trace_stack.is_empty());
        }

        #[test]
        fn test_get_returns_cloned_traces() {
            let mut collector = TraceCollector::default();
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();
            // Pop the trace_stack to simulate call completion
            collector.trace_stack.pop();
            collector.after_op();

            let traces = collector.get();
            assert_eq!(traces.len(), 1);
            assert_eq!(traces[0].len(), 1);
        }
    }

    mod inspector_tests {
        use super::*;

        /// Helper to create a simple test for init_op with create schemes
        #[test]
        fn test_create_scheme_create() {
            let mut collector = TraceCollector::default();

            collector.init_op(
                "create".to_string(),
                "0xaaa".to_string(),
                "".to_string(),
                Bytes::from(vec![0x60, 0x80]), // Simple init code
                "1000".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            let txn = &collector.traces[0];
            assert_eq!(txn.call_type, "create");
            assert_eq!(txn.from, "0xaaa");
            assert_eq!(txn.to, "");
            assert_eq!(txn.input, Bytes::from(vec![0x60, 0x80]));
            assert_eq!(txn.value_wei, "1000");
            assert_eq!(txn.gas, 100000);
        }

        #[test]
        fn test_create_scheme_create2() {
            let mut collector = TraceCollector::default();

            collector.init_op(
                "create2".to_string(),
                "0xaaa".to_string(),
                "".to_string(),
                Bytes::from(vec![0x60, 0x80]),
                "500".to_string(),
                50000,
                "".to_string(),
            );
            collector.before_op();

            let txn = &collector.traces[0];
            assert_eq!(txn.call_type, "create2");
            assert_eq!(txn.from, "0xaaa");
            assert_eq!(txn.value_wei, "500");
        }

        #[test]
        fn test_create_end_sets_contract_address() {
            let mut collector = TraceCollector::default();

            // Simulate a CREATE operation
            collector.init_op(
                "create".to_string(),
                "0xaaa".to_string(),
                "".to_string(),
                Bytes::from(vec![0x60, 0x80]),
                "1000".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // Manually simulate create_end behavior
            let trace_index = collector.trace_stack.pop().unwrap_or_default();
            let created_address = Address::from([0xcc; 20]);

            if let Some(txn) = collector.traces.get_mut(trace_index) {
                txn.to = created_address.to_string();
                txn.gas_used = 25000;
                txn.output = Bytes::from(vec![0x60, 0x60, 0x60, 0x40]);
                txn.is_error = false;
            }

            collector.after_op();

            assert_eq!(collector.all_traces.len(), 1);
            let txn = &collector.all_traces[0][0];
            assert_eq!(txn.to, created_address.to_string());
            assert_eq!(txn.gas_used, 25000);
            assert!(!txn.is_error);
        }

        #[test]
        fn test_create_end_with_error() {
            let mut collector = TraceCollector::default();

            // Simulate a CREATE operation that fails
            collector.init_op(
                "create".to_string(),
                "0xaaa".to_string(),
                "".to_string(),
                Bytes::from(vec![0x60, 0x80]),
                "1000".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // Manually simulate create_end with error
            let trace_index = collector.trace_stack.pop().unwrap_or_default();

            if let Some(txn) = collector.traces.get_mut(trace_index) {
                txn.to = Address::ZERO.to_string();
                txn.gas_used = 100000;
                txn.output = Bytes::default();
                txn.is_error = true;
                txn.error = "out of gas".to_string();
            }

            collector.after_op();

            let txn = &collector.all_traces[0][0];
            assert!(txn.is_error);
            assert_eq!(txn.error, "out of gas");
            assert_eq!(txn.gas_used, 100000);
        }

        #[test]
        fn test_create_with_nested_calls() {
            let mut collector = TraceCollector::default();

            // Parent CREATE
            collector.init_op(
                "create".to_string(),
                "0xaaa".to_string(),
                "".to_string(),
                Bytes::from(vec![0x60, 0x80]),
                "1000".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // Nested CALL from constructor
            collector.init_op(
                "call".to_string(),
                "0xbbb".to_string(),
                "0xccc".to_string(),
                Bytes::default(),
                "0".to_string(),
                50000,
                "".to_string(),
            );
            collector.before_op();

            // End nested call
            collector.trace_stack.pop();
            collector.after_op();

            // End create
            let trace_index = collector.trace_stack.pop().unwrap_or_default();
            if let Some(txn) = collector.traces.get_mut(trace_index) {
                txn.to = Address::from([0xdd; 20]).to_string();
                txn.gas_used = 60000;
                txn.is_error = false;
            }
            collector.after_op();

            assert_eq!(collector.all_traces[0].len(), 2);
            assert_eq!(collector.all_traces[0][0].call_type, "create");
            assert_eq!(collector.all_traces[0][1].call_type, "call");
            assert_eq!(collector.all_traces[0][0].dept, 0);
            assert_eq!(collector.all_traces[0][1].dept, 1);
        }

        #[test]
        fn test_mixed_create_and_call_operations() {
            let mut collector = TraceCollector::default();

            // CALL
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "100".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // CREATE from within the call
            collector.init_op(
                "create2".to_string(),
                "0xbbb".to_string(),
                "".to_string(),
                Bytes::from(vec![0x60, 0x80]),
                "50".to_string(),
                50000,
                "".to_string(),
            );
            collector.before_op();

            // End create2
            let create_idx = collector.trace_stack.pop().unwrap_or_default();
            if let Some(txn) = collector.traces.get_mut(create_idx) {
                txn.to = Address::from([0xdd; 20]).to_string();
                txn.gas_used = 30000;
                txn.is_error = false;
            }
            collector.after_op();

            // End call
            collector.trace_stack.pop();
            collector.after_op();

            assert_eq!(collector.all_traces[0].len(), 2);
            assert_eq!(collector.all_traces[0][0].call_type, "call");
            assert_eq!(collector.all_traces[0][1].call_type, "create2");
        }
    }

    mod scenario_tests {
        use super::*;

        #[test]
        fn test_simple_call_chain() {
            let mut collector = TraceCollector::default();

            // A -> B
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // B -> C (nested)
            collector.init_op(
                "call".to_string(),
                "0xbbb".to_string(),
                "0xccc".to_string(),
                Bytes::default(),
                "0".to_string(),
                50000,
                "".to_string(),
            );
            collector.before_op();

            // C returns
            collector.trace_stack.pop();
            collector.after_op();

            // B returns
            collector.trace_stack.pop();
            collector.after_op();

            assert_eq!(collector.traces.len(), 0);
            assert_eq!(collector.all_traces.len(), 1);
            assert_eq!(collector.all_traces[0].len(), 2);
            assert_eq!(collector.all_traces[0][0].dept, 0);
            assert_eq!(collector.all_traces[0][1].dept, 1);
        }

        #[test]
        fn test_multiple_siblings() {
            let mut collector = TraceCollector::default();

            // A calls B, C, D as siblings
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // First sibling call
            collector.init_op(
                "call".to_string(),
                "0xbbb".to_string(),
                "0xccc".to_string(),
                Bytes::default(),
                "0".to_string(),
                50000,
                "".to_string(),
            );
            collector.before_op();
            collector.trace_stack.pop();
            collector.after_op();

            // Second sibling call
            collector.init_op(
                "staticcall".to_string(),
                "0xbbb".to_string(),
                "0xddd".to_string(),
                Bytes::default(),
                "0".to_string(),
                50000,
                "".to_string(),
            );
            collector.before_op();
            collector.trace_stack.pop();
            collector.after_op();

            // Third sibling call
            collector.init_op(
                "delegatecall".to_string(),
                "0xbbb".to_string(),
                "0xeee".to_string(),
                Bytes::default(),
                "0".to_string(),
                50000,
                "0xfff".to_string(),
            );
            collector.before_op();
            collector.trace_stack.pop();
            collector.after_op();

            // Parent returns
            collector.trace_stack.pop();
            collector.after_op();

            assert_eq!(collector.all_traces[0].len(), 4);
            assert_eq!(collector.all_traces[0][0].name, "call");
            assert_eq!(collector.all_traces[0][1].name, "call_0");
            assert_eq!(collector.all_traces[0][2].name, "staticcall_1");
            assert_eq!(collector.all_traces[0][3].name, "delegatecall_2");
        }

        #[test]
        fn test_deep_nesting() {
            let mut collector = TraceCollector::default();

            // Create 5 levels of nesting
            for i in 0..5 {
                collector.init_op(
                    "call".to_string(),
                    format!("0x{i:x}"),
                    format!("0x{:x}", i + 1),
                    Bytes::default(),
                    "0".to_string(),
                    100000,
                    "".to_string(),
                );
                collector.before_op();
            }

            // Verify depth is tracked correctly
            for i in 0..5 {
                assert_eq!(collector.traces[i].dept, i as u64);
            }

            // Unwind the stack
            for _ in 0..5 {
                collector.trace_stack.pop();
                collector.after_op();
            }

            assert_eq!(collector.all_traces.len(), 1);
            assert_eq!(collector.all_traces[0].len(), 5);
        }

        #[test]
        fn test_trace_naming_with_complex_hierarchy() {
            let mut collector = TraceCollector::default();

            // Level 0
            collector.init_op(
                "call".to_string(),
                "0xa".to_string(),
                "0xb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();

            // Level 1, index 0
            collector.init_op(
                "call".to_string(),
                "0xb".to_string(),
                "0xc".to_string(),
                Bytes::default(),
                "0".to_string(),
                50000,
                "".to_string(),
            );
            collector.before_op();

            // Level 2, index 0
            collector.init_op(
                "staticcall".to_string(),
                "0xc".to_string(),
                "0xd".to_string(),
                Bytes::default(),
                "0".to_string(),
                25000,
                "".to_string(),
            );
            collector.before_op();

            assert_eq!(collector.traces[0].name, "call");
            assert_eq!(collector.traces[1].name, "call_0");
            assert_eq!(collector.traces[2].name, "staticcall_0_0");
        }

        #[test]
        fn test_selfdestruct_trace() {
            let mut collector = TraceCollector::default();

            let contract = Address::from([0x11; 20]);
            let target = Address::from([0x22; 20]);
            let value = U256::from(1000);

            // Manually test the selfdestruct logic without calling the Inspector trait method
            collector.init_op(
                "selfdestruct".to_string(),
                contract.to_string(),
                target.to_string(),
                Bytes::default(),
                value.to_string(),
                0,
                "".to_string(),
            );
            collector.before_op();
            collector.trace_stack.pop();
            collector.after_op();

            assert_eq!(collector.all_traces.len(), 1);
            let txn = &collector.all_traces[0][0];
            assert_eq!(txn.call_type, "selfdestruct");
            assert_eq!(txn.from, contract.to_string());
            assert_eq!(txn.to, target.to_string());
            assert_eq!(txn.value_wei, "1000");
        }

        #[test]
        fn test_multiple_transactions_tracked_separately() {
            let mut collector = TraceCollector::default();

            // First transaction
            collector.init_op(
                "call".to_string(),
                "0xaaa".to_string(),
                "0xbbb".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();
            collector.trace_stack.pop();
            collector.after_op();

            // Second transaction
            collector.init_op(
                "call".to_string(),
                "0xccc".to_string(),
                "0xddd".to_string(),
                Bytes::default(),
                "0".to_string(),
                100000,
                "".to_string(),
            );
            collector.before_op();
            collector.trace_stack.pop();
            collector.after_op();

            assert_eq!(collector.all_traces.len(), 2);
            assert_eq!(collector.all_traces[0][0].from, "0xaaa");
            assert_eq!(collector.all_traces[1][0].from, "0xccc");
        }
    }
}
