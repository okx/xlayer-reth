use std::sync::Arc;
use std::str::FromStr;

use futures::future::Either;
use jsonrpsee::{
    core::middleware::{Batch, Notification},
    server::middleware::rpc::RpcServiceT,
    types::Request,
    MethodResponse,
};
use jsonrpsee_types::Id;
use serde_json::value::RawValue;
use tower::Layer;
use tracing::{debug, info};
use alloy_primitives::{Address, Bytes, B256, U256};
use thiserror::Error;

/// Prefix character for X Layer addresses
const X_ADDRESS_PREFIX: &str = "XKO";

/// Address type that handles addresses with an optional "XKO" prefix
/// The "XKO" prefix is stripped when converting to Address for chain operations
/// Examples: "XKO1234..." or "0x1234..." or "1234..." (all valid)
#[derive(Debug, Clone)]
pub struct XAddress {
    address: Address,
}

/// Error type for XAddress parsing
#[derive(Debug, Error)]
pub enum XAddressError {
    /// Failed to parse address
    #[error("Failed to parse address: {0}")]
    ParseError(String),
    /// Invalid address format
    #[error("Invalid address format: {0}")]
    InvalidFormat(String),
}

impl FromStr for XAddress {
    type Err = XAddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let addr_str = s.strip_prefix(X_ADDRESS_PREFIX).unwrap_or(s);
        let addr_with_prefix =
            if addr_str.starts_with("0x") { addr_str.to_string() } else { format!("0x{addr_str}") };

        let address = addr_with_prefix
            .parse::<Address>()
            .map_err(|e| XAddressError::ParseError(format!("Failed to parse address '{}': {}", addr_with_prefix, e)))?;

        Ok(Self { address })
    }
}

impl From<XAddress> for Address {
    fn from(x_addr: XAddress) -> Self {
        x_addr.address
    }
}

impl std::fmt::Display for XAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.address)
    }
}

/// Layer that creates the address modification middleware
#[derive(Clone)]
pub struct AddressMiddlewareLayer {
}

impl AddressMiddlewareLayer {
    pub fn new() -> Self {
        info!("starting multi address middleware layer");
        Self { }
    }
}

impl<S> Layer<S> for AddressMiddlewareLayer {
    type Service = AddressMiddlewareService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        info!(target: "xlayer::address_middleware", "AddressMiddlewareLayer::layer() called");
        AddressMiddlewareService {
            inner,
        }
    }
}

/// Address modification middleware service
#[derive(Clone)]
pub struct AddressMiddlewareService<S> {
    inner: S,
}

impl<S> AddressMiddlewareService<S> {
    /// Modify address in request parameters
    fn modify_address_in_params<'a>(
        &self,
        req: &Request<'a>,
        address_index: usize,
    ) -> Option<Request<'a>> {
        info!("modify_address_in_params");
        let _p = req.params(); // keeps compiler quiet
        let params_str = _p.as_str()?;
        let mut parsed: serde_json::Value = serde_json::from_str(params_str).ok()?;

        let arr = parsed.as_array_mut()?;
        if address_index >= arr.len() {
            return None;
        }

        let address_value = arr.get_mut(address_index)?;
        let address_str = address_value.as_str()?;

        let x_addr = XAddress::from_str(address_str).expect("Should parse 0x-prefixed address");
        let address: Address = x_addr.into();
        let modified_address = address.to_string();

        *address_value = serde_json::Value::String(modified_address);

        let new_params_str = serde_json::to_string(&parsed).ok()?;

        let params_raw = RawValue::from_string(new_params_str).ok()?;

        Some(Request::owned(
            req.method_name().to_string(),
            Some(params_raw),
            req.id(),
        ))
    }
}

impl<S> RpcServiceT for AddressMiddlewareService<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync + Clone + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method = req.method_name();
        let needs_modification = matches!(method, "eth_getBalance" | "eth_getTransactionCount");

        if !needs_modification {
            return Either::Left(self.inner.call(req));
        }

        // Address is the first parameter (index 0) for both methods
        let modified_req = match self.modify_address_in_params(&req, 0) {
            Some(modified) => {
                debug!(
                    target: "xlayer::address_middleware",
                    "Modified address for method: {}",
                    method
                );
                modified
            }
            None => {
                debug!(
                    target: "xlayer::address_middleware",
                    "Failed to modify address for method: {}, forwarding original request",
                    method
                );
                return Either::Left(self.inner.call(req));
            }
        };

        let inner = self.inner.clone();
        Either::Right(Box::pin(async move {
            inner.call(modified_req).await
        }))
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        self.inner.batch(req)
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;

    #[test]
    fn test_xaddress_from_str_with_xko_prefix() {
        let addr_str = "XKO70586BeEB7b7Aa2e7966DF9c8493C6CbFd75C625";
        let x_addr = XAddress::from_str(addr_str).expect("Should parse XKO-prefixed address");
        let address: Address = x_addr.into();
        
        let expected = Address::from_str("0x70586BeEB7b7Aa2e7966DF9c8493C6CbFd75C625").unwrap();
        assert_eq!(address, expected);
    }

    #[test]
    fn test_xaddress_from_str_with_0x_prefix() {
        let addr_str = "0x70586BeEB7b7Aa2e7966DF9c8493C6CbFd75C625";
        let x_addr = XAddress::from_str(addr_str).expect("Should parse 0x-prefixed address");
        let address: Address = x_addr.into();

        let expected = Address::from_str("0x70586BeEB7b7Aa2e7966DF9c8493C6CbFd75C625").unwrap();
        assert_eq!(address, expected);
    }

    #[test]
    fn test_xaddress_from_str_without_0x_prefix() {
        let addr_str = "70586BeEB7b7Aa2e7966DF9c8493C6CbFd75C625";
        let x_addr = XAddress::from_str(addr_str).expect("Should parse 0x-prefixed address");
        let address: Address = x_addr.into();

        let expected = Address::from_str("0x70586BeEB7b7Aa2e7966DF9c8493C6CbFd75C625").unwrap();
        assert_eq!(address, expected);
    }

    #[test]
    fn test_xaddress_from_str_all_lowercase() {
        let addr_str = "70586beeb7b7aa2e7966df9c8493c6cbfd75c625";
        let x_addr = XAddress::from_str(addr_str).expect("Should parse lowercase address");
        let address: Address = x_addr.into();

        let expected = Address::from_str("0x70586BeEB7b7Aa2e7966DF9c8493C6CbFd75C625").unwrap();
        assert_eq!(address, expected);
    }
}