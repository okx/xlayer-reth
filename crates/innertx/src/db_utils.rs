use std::fmt;

use alloy_primitives::{BlockHash, TxHash};
use alloy_rlp::{decode_exact, encode, Encodable};
use eyre::Report;
use once_cell::sync::OnceCell;

use crate::innertx_inspector::InternalTransaction;
use reth_db::{
    mdbx::init_db_for,
    mdbx::{Database, DatabaseArguments, Transaction, WriteFlags, RW},
    DatabaseEnv,
};
use reth_db_api::{
    models::ClientVersion,
    table::{Table, TableInfo},
    tables, TableSet, TableType, TableViewer,
};

static XLAYERDB: OnceCell<DatabaseEnv> = OnceCell::new();

reth_db_api::tables! {
    /// Maps transaction hash to vector of internal transactions
    /// Key: TxHash (as Vec<u8>)
    /// Value: Vec<InternalTransaction> (RLP encoded as Vec<u8>)
    table TxTable {
        type Key = Vec<u8>;
        type Value = Vec<u8>;
    }

    /// Maps block hash to vector of transaction hashes in that block
    /// Key: BlockHash (as Vec<u8>)
    /// Value: Vec<TxHash> (RLP encoded as Vec<u8>)
    table BlockTable {
        type Key = Vec<u8>;
        type Value = Vec<u8>;
    }
}

pub fn initialize_inner_tx_db(db_path: &str) -> Result<(), Report> {
    let db_dir = format!("{}/{}", db_path, "xlayerdb");

    let db = init_db_for::<_, Tables>(&db_dir, DatabaseArguments::new(ClientVersion::default()))?;

    XLAYERDB.set(db).map_err(|_| Report::msg("xlayerdb was initialized more than once"))?;

    Ok(())
}

pub fn write_single<T: Table, P: Encodable + std::fmt::Debug>(
    key: Vec<u8>,
    value: P,
) -> Result<(), Report> {
    let txn_begin_result = XLAYERDB.get().unwrap().begin_rw_txn();
    if let Err(err) = txn_begin_result {
        return Err(Into::<Report>::into(err).wrap_err("write single txn begin failed"));
    }

    let txn = txn_begin_result.unwrap();

    let txn_opendb_result = txn.open_db(Some(T::NAME));
    if let Err(err) = txn_opendb_result {
        return Err(Into::<Report>::into(err).wrap_err("write single txn open db failed"));
    }

    let table = txn_opendb_result.unwrap();
    let encoded_bytes = encode(&value);

    let txn_put_result = txn.put(table.dbi(), &key, encoded_bytes, WriteFlags::default());
    if let Err(err) = txn_put_result {
        return Err(Into::<Report>::into(err).wrap_err(format!(
            "write single txn put failed with key {:#?} and value {:#?}",
            &key, &value
        )));
    }

    let txn_commit_result = txn.commit();
    if let Err(err) = txn_commit_result {
        return Err(Into::<Report>::into(err).wrap_err("write single txn commit failed"));
    }

    Ok(())
}

pub fn read_single<T: Table>(key: Vec<u8>) -> Result<Vec<u8>, Report> {
    let txn_begin_result = XLAYERDB.get().unwrap().begin_ro_txn();
    if let Err(err) = txn_begin_result {
        return Err(Into::<Report>::into(err).wrap_err("read single txn begin failed"));
    }

    let txn = txn_begin_result.unwrap();

    let txn_opendb_result = txn.open_db(Some(T::NAME));
    if let Err(err) = txn_opendb_result {
        return Err(Into::<Report>::into(err).wrap_err("read single txn open db failed"));
    }

    let table = txn_opendb_result.unwrap();

    let txn_get_result = txn.get(table.dbi(), &key);
    if let Err(err) = txn_get_result {
        return Err(Into::<Report>::into(err)
            .wrap_err(format!("read single txn get failed with key {:#?}", &key)));
    }

    Ok(txn_get_result.unwrap().unwrap_or_default())
}

pub fn delete_single<T: Table>(key: Vec<u8>) -> Result<(), Report> {
    let txn_begin_result = XLAYERDB.get().unwrap().begin_rw_txn();
    if let Err(err) = txn_begin_result {
        return Err(Into::<Report>::into(err).wrap_err("delete single txn begin failed"));
    }

    let txn = txn_begin_result.unwrap();

    let txn_opendb_result = txn.open_db(Some(T::NAME));
    if let Err(err) = txn_opendb_result {
        return Err(Into::<Report>::into(err).wrap_err("delete single txn open db failed"));
    }

    let table = txn_opendb_result.unwrap();

    let txn_delete_result = txn.del(table.dbi(), &key, None);
    if let Err(err) = txn_delete_result {
        return Err(Into::<Report>::into(err)
            .wrap_err(format!("delete single txn put failed with key {:#?}", &key)));
    }

    let txn_commit_result = txn.commit();
    if let Err(err) = txn_commit_result {
        return Err(Into::<Report>::into(err).wrap_err("delete single txn commit failed"));
    }

    Ok(())
}

pub fn rw_batch_start<T: Table>() -> Result<(Transaction<RW>, Database), Report> {
    let txn_begin_result = XLAYERDB.get().unwrap().begin_rw_txn();
    if let Err(err) = txn_begin_result {
        return Err(Into::<Report>::into(err).wrap_err("rw batch start begin failed"));
    }

    let txn = txn_begin_result.unwrap();

    let txn_opendb_result = txn.open_db(Some(T::NAME));
    if let Err(err) = txn_opendb_result {
        return Err(Into::<Report>::into(err).wrap_err("rw batch start open db failed"));
    }

    Ok((txn, txn_opendb_result.unwrap()))
}

pub fn rw_batch_write<T: Table>(
    txn: &Transaction<RW>,
    table: &Database,
    key: Vec<u8>,
    value: Vec<u8>,
) -> Result<(), Report> {
    let txn_put_result = txn.put(table.dbi(), &key, &value, WriteFlags::default());
    if let Err(err) = txn_put_result {
        return Err(Into::<Report>::into(err).wrap_err(format!(
            "rw batch write failed with key {:#?} and value {:#?}",
            &key, &value
        )));
    }

    Ok(())
}

pub fn rw_batch_delete<T: Table>(
    txn: &Transaction<RW>,
    table: &Database,
    key: Vec<u8>,
) -> Result<(), Report> {
    let txn_del_result = txn.del(table.dbi(), &key, None);
    if let Err(err) = txn_del_result {
        return Err(Into::<Report>::into(err)
            .wrap_err(format!("rw batch delete failed with key {:#?}", &key)));
    }

    Ok(())
}

pub fn rw_batch_end<T: Table>(txn: Transaction<RW>) -> Result<(), Report> {
    let txn_commit_result = txn.commit();
    if let Err(err) = txn_commit_result {
        return Err(Into::<Report>::into(err).wrap_err("rw batch end commit failed"));
    }

    Ok(())
}

pub fn read_table_tx(tx_hash: TxHash) -> Result<Vec<InternalTransaction>, Report> {
    let read_result = read_single::<TxTable>(tx_hash.to_vec());
    if let Err(err) = read_result {
        return Err(err.wrap_err(format!("tx table read failed with tx_hash {:#?}", &tx_hash)));
    }

    let encoded_result = read_result.unwrap();
    if encoded_result.is_empty() {
        return Ok(Vec::<InternalTransaction>::default());
    }

    let decode_result = decode_exact(&encoded_result);
    if let Err(err) = decode_result {
        return Err(Into::<Report>::into(err).wrap_err(format!(
            "tx table decode failed with encoded result {:#?}",
            &encoded_result
        )));
    }

    Ok(decode_result.unwrap())
}

pub fn read_table_block(block_hash: BlockHash) -> Result<Vec<TxHash>, Report> {
    let read_result = read_single::<BlockTable>(block_hash.to_vec());
    if let Err(err) = read_result {
        return Err(
            err.wrap_err(format!("block table read failed with block_hash {:#?}", &block_hash))
        );
    }

    let encoded_result = read_result.unwrap();
    if encoded_result.is_empty() {
        return Ok(Vec::<TxHash>::default());
    }

    let decode_result = decode_exact(&encoded_result);
    if let Err(err) = decode_result {
        return Err(Into::<Report>::into(err).wrap_err(format!(
            "block table decode failed with encoded result {:#?}",
            &encoded_result
        )));
    }

    Ok(decode_result.unwrap())
}
