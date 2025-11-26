use jsonrpsee::{core::middleware::RpcServiceT, types::Request, MethodResponse};
use jsonrpsee_types::Id;
use serde_json::value::RawValue;

use crate::LegacyRpcRouterService;

impl<S> LegacyRpcRouterService<S> {
    pub async fn call_eth_get_block_by_hash(
        &self,
        block_hash: &str,
        full_transactions: bool,
    ) -> Result<Option<u64>, serde_json::Error>
    where
        S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
    {
        // Construct the parameters JSON string
        let params_str = format!(r#"["{}", {}]"#, block_hash, full_transactions);

        let method = "eth_getBlockByHash";
        let params_raw = RawValue::from_string(params_str).expect("Valid JSON params");
        let id = Id::Number(1);

        // Create request using borrowed data
        let request = Request::owned(method.into(), Some(params_raw), id);

        // Call inner service
        let res = self.inner.call(request).await;

        let response = serde_json::from_str::<serde_json::Value>(res.as_json().get())?;
        let block_num = response
            .get("result")
            .and_then(|result| result.get("number"))
            .and_then(|n| n.as_str())
            .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok());

        Ok(block_num)
    }
}
