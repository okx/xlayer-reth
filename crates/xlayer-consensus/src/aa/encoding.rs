//! RLP helpers shared by the AA wire types.
//!
//! Hand-rolled (rather than derive-based) because two of the helpers need to
//! stay stable across both signing and non-signing preimages:
//!
//! - [`encode_optional_address`] / [`decode_optional_address`] treat
//!   `None` as the empty RLP string (`0x80`), matching tempo's convention.
//! - [`encode_nested_calls`] / [`decode_nested_calls`] preserve phase
//!   boundaries (`Vec<Vec<Call>>`) so EIP-8130's atomic-per-phase semantics
//!   survive round-tripping.
//!
//! Kept in a dedicated module so [`super::tx`] only holds field ordering.

use std::vec::Vec;

use alloy_primitives::Address;
use alloy_rlp::{length_of_length, BufMut, Decodable, Encodable, Header};

use super::Call;

/// Encodes a slice of RLP items as an RLP list.
pub(crate) fn encode_list<T: Encodable>(items: &[T], out: &mut dyn BufMut) {
    let payload_len: usize = items.iter().map(Encodable::length).sum();
    Header { list: true, payload_length: payload_len }.encode(out);
    for item in items {
        item.encode(out);
    }
}

/// Full RLP length (header + payload) of a slice encoded via [`encode_list`].
pub(crate) fn list_len<T: Encodable>(items: &[T]) -> usize {
    let payload_len: usize = items.iter().map(Encodable::length).sum();
    payload_len + length_of_length(payload_len)
}

/// Encodes an `Option<Address>` — `None` as empty-string (`0x80`), `Some` as a
/// 20-byte RLP string.
pub(crate) fn encode_optional_address(addr: &Option<Address>, out: &mut dyn BufMut) {
    match addr {
        Some(address) => address.encode(out),
        None => out.put_u8(0x80),
    }
}

/// RLP length of an `Option<Address>` encoded via [`encode_optional_address`].
pub(crate) fn optional_address_len(addr: &Option<Address>) -> usize {
    match addr {
        Some(address) => address.length(),
        None => 1,
    }
}

/// Decodes an `Option<Address>` from the RLP empty-string / 20-byte-string
/// convention.
pub(crate) fn decode_optional_address(buf: &mut &[u8]) -> alloy_rlp::Result<Option<Address>> {
    let header = Header::decode(buf)?;
    if header.list {
        return Err(alloy_rlp::Error::UnexpectedList);
    }
    match header.payload_length {
        0 => Ok(None),
        20 => {
            if buf.len() < 20 {
                return Err(alloy_rlp::Error::UnexpectedLength);
            }
            let address = Address::from_slice(&buf[..20]);
            *buf = &buf[20..];
            Ok(Some(address))
        }
        _ => Err(alloy_rlp::Error::UnexpectedLength),
    }
}

/// Encodes `Vec<Vec<Call>>` as an RLP list-of-lists (phase boundaries
/// preserved).
pub(crate) fn encode_nested_calls(phases: &[Vec<Call>], out: &mut dyn BufMut) {
    let payload_len: usize = phases.iter().map(|phase| list_len(phase.as_slice())).sum();
    Header { list: true, payload_length: payload_len }.encode(out);
    for phase in phases {
        encode_list(phase, out);
    }
}

/// RLP length of a `Vec<Vec<Call>>` encoded via [`encode_nested_calls`].
pub(crate) fn nested_calls_len(phases: &[Vec<Call>]) -> usize {
    let payload_len: usize = phases.iter().map(|phase| list_len(phase.as_slice())).sum();
    payload_len + length_of_length(payload_len)
}

/// Decodes a `Vec<Vec<Call>>` previously encoded via [`encode_nested_calls`].
pub(crate) fn decode_nested_calls(buf: &mut &[u8]) -> alloy_rlp::Result<Vec<Vec<Call>>> {
    let outer = Header::decode(buf)?;
    if !outer.list {
        return Err(alloy_rlp::Error::UnexpectedString);
    }
    let outer_end = buf.len() - outer.payload_length;
    let mut phases = Vec::new();
    while buf.len() > outer_end {
        let inner = Header::decode(buf)?;
        if !inner.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let inner_end = buf.len() - inner.payload_length;
        let mut calls = Vec::new();
        while buf.len() > inner_end {
            calls.push(Call::decode(buf)?);
        }
        phases.push(calls);
    }
    Ok(phases)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, Bytes};

    use super::*;

    #[test]
    fn optional_address_round_trip() {
        let mut empty_buf = Vec::new();
        encode_optional_address(&None, &mut empty_buf);
        assert_eq!(empty_buf, vec![0x80]);
        assert_eq!(decode_optional_address(&mut empty_buf.as_slice()).unwrap(), None);

        let address = Address::repeat_byte(0xAB);
        let mut address_buf = Vec::new();
        encode_optional_address(&Some(address), &mut address_buf);
        assert_eq!(optional_address_len(&Some(address)), address_buf.len());
        assert_eq!(decode_optional_address(&mut address_buf.as_slice()).unwrap(), Some(address));
    }

    #[test]
    fn nested_calls_round_trip() {
        let phases = vec![
            vec![
                Call { to: Address::repeat_byte(1), data: Bytes::from_static(&[0x01]) },
                Call { to: Address::repeat_byte(2), data: Bytes::from_static(&[0x02]) },
            ],
            vec![Call { to: Address::repeat_byte(3), data: Bytes::from_static(&[0x03]) }],
        ];
        let mut buf = Vec::new();
        encode_nested_calls(&phases, &mut buf);
        assert_eq!(nested_calls_len(&phases), buf.len());

        let decoded = decode_nested_calls(&mut buf.as_slice()).unwrap();
        assert_eq!(decoded, phases);
    }
}
