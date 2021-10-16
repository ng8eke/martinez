use martinez::{
    accessors::{
        chain::*,
        state::{account, storage},
    },
    binutil::MartinezDataDir,
    consensus::engine_factory,
    execution::{address::create_address, analysis_cache::*, evm::execute, processor::*},
    kv::traits::*,
    models::*,
    res::chainspec::MAINNET,
    stagedsync::stages::*,
    Buffer, IntraBlockState,
};
use async_trait::async_trait;
use bytes::Bytes;
use clap::Parser;
use ethereum_types::{Address, U64};
use ethnum::U256;
use jsonrpc::common::{CallData, StoragePos, TxIndex, TxLog, TxReceipt, UncleIndex};
use jsonrpsee::{
    core::{Error::*, RpcResult},
    http_server::HttpServerBuilder,
    proc_macros::rpc,
};
use std::{future::pending, net::SocketAddr, sync::Arc};
use tracing_subscriber::{prelude::*, EnvFilter};

#[derive(Parser)]
#[clap(name = "Martinez RPC", about = "RPC server for Martinez")]
pub struct Opt {
    #[clap(long)]
    pub datadir: MartinezDataDir,

    #[clap(long)]
    pub listen_address: SocketAddr,
}

#[rpc(server, namespace = "eth")]
pub trait EthApi {
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> RpcResult<BlockNumber>;
    #[method(name = "call")]
    async fn call(&self, call_data: CallData, block_number: BlockNumber) -> RpcResult<Bytes>;
    #[method(name = "estimateGas")]
    async fn estimate_gas(&self, call_data: CallData, block_number: BlockNumber) -> RpcResult<U64>;
    #[method(name = "getBalance")]
    async fn get_balance(&self, address: Address, block_number: BlockNumber) -> RpcResult<U256>;
    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(
        &self,
        block_hash: H256,
        full_tx_obj: bool,
    ) -> RpcResult<jsonrpc::common::Block>;
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        block_number: BlockNumber,
        full_tx_obj: bool,
    ) -> RpcResult<jsonrpc::common::Block>;
    #[method(name = "getBlockTransactionCountByHash")]
    async fn get_block_tx_count_by_hash(&self, block_hash: H256) -> RpcResult<U64>;
    #[method(name = "getBlockTransactionCountByNumber")]
    async fn get_block_tx_count_by_number(&self, block_number: BlockNumber) -> RpcResult<U64>;
    #[method(name = "getCode")]
    async fn get_code(&self, address: Address, block_number: BlockNumber) -> RpcResult<Bytes>;
    #[method(name = "getStorageAt")]
    async fn get_storage_at(
        &self,
        address: Address,
        storage_pos: StoragePos,
        block_number: BlockNumber,
    ) -> RpcResult<U256>;
    #[method(name = "getTransactionByBlockHashAndIndex")]
    async fn get_tx_by_block_hash_and_index(
        &self,
        block_hash: H256,
        index: TxIndex,
    ) -> RpcResult<jsonrpc::common::Tx>;
    #[method(name = "getTransactionByBlockNumberAndIndex")]
    async fn get_tx_by_block_number_and_index(
        &self,
        block_number: BlockNumber,
        index: TxIndex,
    ) -> RpcResult<jsonrpc::common::Tx>;
    #[method(name = "getTransactionCount")]
    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: BlockNumber,
    ) -> RpcResult<U64>;
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(&self, tx_hash: H256) -> RpcResult<TxReceipt>;
    #[method(name = "getUncleByBlockHashAndIndex")]
    async fn get_uncle_by_block_hash_and_index(
        &self,
        block_hash: H256,
        index: UncleIndex,
    ) -> RpcResult<jsonrpc::common::Block>;
    #[method(name = "getUncleByBlockNumberAndIndex")]
    async fn get_uncle_by_block_number_and_index(
        &self,
        block_number: BlockNumber,
        index: UncleIndex,
    ) -> RpcResult<jsonrpc::common::Block>;
    #[method(name = "getUncleCountByBlockHash")]
    async fn get_uncle_count_by_block_hash(&self, block_hash: H256) -> RpcResult<U64>;
    #[method(name = "getUncleCountByBlockNumber")]
    async fn get_uncle_count_by_block_number(&self, block_number: BlockNumber) -> RpcResult<U64>;
}

pub struct EthApiServerImpl<DB>
where
    DB: KV,
{
    db: Arc<DB>,
}

#[async_trait]
impl<DB> EthApiServer for EthApiServerImpl<DB>
where
    DB: KV,
{
    async fn block_number(&self) -> RpcResult<BlockNumber> {
        Ok(FINISH
            .get_progress(&self.db.begin().await?)
            .await?
            .unwrap_or_default())
    }

    async fn call(&self, call_data: CallData, block_number: BlockNumber) -> RpcResult<Bytes> {
        let tx = &self.db.begin().await?;

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        let header = header::read(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let mut state = Buffer::new(tx, BlockNumber(0), Some(block_number));
        let mut analysis_cache = AnalysisCache::default();
        let block_spec = MAINNET.collect_block_spec(block_number);

        let value = match call_data.value {
            None => Default::default(),
            Some(v) => v,
        };

        let input = match call_data.data {
            None => Default::default(),
            Some(i) => i,
        };

        let sender = match call_data.from {
            None => Default::default(),
            Some(s) => s,
        };

        let msg_with_sender = MessageWithSender {
            message: Message::Legacy {
                chain_id: Some(ChainId(1)),
                nonce: 0,
                gas_price: Default::default(),
                gas_limit: 0,
                action: TransactionAction::Call(call_data.to),
                value,
                input,
            },
            sender,
        };

        let gas = match call_data.gas {
            None => Default::default(),
            Some(g) => g.low_u64(),
        };

        Ok(execute(
            &mut IntraBlockState::new(&mut state),
            None,
            &mut analysis_cache,
            &PartialHeader::from(header.clone()),
            &block_spec,
            &msg_with_sender,
            gas,
        )
        .await?
        .output_data)
    }

    async fn estimate_gas(&self, call_data: CallData, block_number: BlockNumber) -> RpcResult<U64> {
        let tx = &self.db.begin().await?;

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        let nonce = account::read(tx, call_data.from.unwrap_or_default(), Some(block_number))
            .await?
            .map(|acc| acc.nonce)
            .unwrap_or_default();

        let value = match call_data.value {
            None => Default::default(),
            Some(v) => v,
        };

        let input = match call_data.data {
            None => Default::default(),
            Some(i) => i,
        };

        let sender = match call_data.from {
            None => Default::default(),
            Some(s) => s,
        };

        // TODO: retrieval of pending block to set gas_limit if gas not supplied
        let gas_limit = match call_data.gas {
            None => return Err(Request("gas must be manually set".to_owned())),
            Some(g) => g.low_u64(),
        };

        let msg = MessageWithSender {
            message: Message::Legacy {
                chain_id: Some(ChainId(1)),
                nonce,
                gas_price: Default::default(),
                gas_limit,
                action: TransactionAction::Call(call_data.to),
                value,
                input,
            },
            sender,
        };

        let block = BlockBodyWithSenders {
            transactions: vec![msg],
            ommers: vec![],
        };

        let header = &PartialHeader::from(
            header::read(tx, block_hash, block_number)
                .await?
                .unwrap_or_default(),
        );

        let mut state = Buffer::new(tx, BlockNumber(0), Some(block_number));
        let mut analysis_cache = AnalysisCache::default();
        let mut engine = engine_factory(MAINNET.clone()).unwrap();
        let block_spec = MAINNET.collect_block_spec(block_number);
        let mut processor = ExecutionProcessor::new(
            &mut state,
            None,
            &mut analysis_cache,
            &mut *engine,
            header,
            &block,
            &block_spec,
        );

        let mut receipts = processor.execute_block_no_post_validation().await?;

        Ok(U64::from(
            receipts.pop().map(|r| r.cumulative_gas_used).unwrap_or(0),
        ))
    }

    async fn get_balance(&self, address: Address, block_number: BlockNumber) -> RpcResult<U256> {
        Ok(
            account::read(&self.db.begin().await?, address, Some(block_number))
                .await?
                .map(|acc| acc.balance)
                .unwrap_or(U256::ZERO),
        )
    }

    async fn get_block_by_hash(
        &self,
        block_hash: H256,
        full_tx_obj: bool,
    ) -> RpcResult<jsonrpc::common::Block> {
        let tx = &self.db.begin().await?;

        let block_number = header_number::read(tx, block_hash)
            .await?
            .unwrap_or_default();

        Ok(json_obj::assemble_block(tx, block_hash, block_number, full_tx_obj).await?)
    }

    async fn get_block_by_number(
        &self,
        block_number: BlockNumber,
        full_tx_obj: bool,
    ) -> RpcResult<jsonrpc::common::Block> {
        let tx = &self.db.begin().await?;

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        Ok(json_obj::assemble_block(tx, block_hash, block_number, full_tx_obj).await?)
    }

    async fn get_block_tx_count_by_hash(&self, block_hash: H256) -> RpcResult<U64> {
        let tx = &self.db.begin().await?;

        let block_number = header_number::read(tx, block_hash)
            .await?
            .unwrap_or_default();

        let block_body = storage_body::read(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        Ok(U64::from(block_body.tx_amount))
    }

    async fn get_block_tx_count_by_number(&self, block_number: BlockNumber) -> RpcResult<U64> {
        let tx = &self.db.begin().await?;

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        let block_body = storage_body::read(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        Ok(U64::from(block_body.tx_amount))
    }

    async fn get_code(&self, address: Address, block_number: BlockNumber) -> RpcResult<Bytes> {
        let tx = &self.db.begin().await?;

        let code_hash = account::read(tx, address, Some(block_number))
            .await?
            .unwrap_or_default()
            .code_hash;

        Ok(
            martinez::read_account_code(tx, Address::from([1; 20]), code_hash)
                .await?
                .unwrap_or_default(),
        )
    }

    async fn get_storage_at(
        &self,
        address: Address,
        storage_pos: StoragePos,
        block_number: BlockNumber,
    ) -> RpcResult<U256> {
        Ok(storage::read(
            &self.db.begin().await?,
            address,
            storage_pos,
            Some(block_number),
        )
        .await?)
    }

    async fn get_tx_by_block_hash_and_index(
        &self,
        block_hash: H256,
        index: TxIndex,
    ) -> RpcResult<jsonrpc::common::Tx> {
        let tx = &self.db.begin().await?;

        let block_number = header_number::read(tx, block_hash)
            .await?
            .unwrap_or_default();

        let block_body = block_body::read_without_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let i = index.as_u64() as usize;
        let msgs = block_body.transactions;

        Ok(json_obj::assemble_tx(block_hash, block_number, &msgs[i], i))
    }

    async fn get_tx_by_block_number_and_index(
        &self,
        block_number: BlockNumber,
        index: TxIndex,
    ) -> RpcResult<jsonrpc::common::Tx> {
        let tx = &self.db.begin().await?;

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        let block_body = block_body::read_without_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let i = index.as_u64() as usize;
        let msgs = block_body.transactions;

        Ok(json_obj::assemble_tx(block_hash, block_number, &msgs[i], i))
    }

    async fn get_transaction_count(
        &self,
        address: Address,
        block_number: BlockNumber,
    ) -> RpcResult<U64> {
        Ok(U64::from(
            account::read(&self.db.begin().await?, address, Some(block_number))
                .await?
                .map(|acc| acc.nonce)
                .unwrap_or_default(),
        ))
    }

    async fn get_transaction_receipt(&self, tx_hash: H256) -> RpcResult<TxReceipt> {
        let tx = &self.db.begin().await?;

        let block_number = tl::read(tx, tx_hash).await?.unwrap_or_default();

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        let msgs_with_sender = block_body::read_with_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default()
            .transactions;

        let msgs: Vec<(usize, MessageWithSender)> = msgs_with_sender
            .into_iter()
            .filter(|msg| msg.hash() == tx_hash)
            .enumerate()
            .map(|(index, msg)| (index, msg))
            .collect();
        let (index, msg) = msgs[0].clone();
        /*let msgs: Vec<(usize, &MessageWithSender)> = msgs_with_sender
            .iter()
            .filter(|msg| msg.hash() == tx_hash)
            .enumerate()
            .map(|(index, msg)| (index, msg))
            .collect();
        let (index, msg) = msgs[0];*/

        let header = header::read(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let mut state = Buffer::new(tx, BlockNumber(0), Some(BlockNumber(block_number.0 - 1)));

        let partial_header = &PartialHeader::from(header.clone());

        let block = block_body::read_with_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let mut analysis_cache = AnalysisCache::default();
        let mut engine = engine_factory(MAINNET.clone()).unwrap();
        let block_spec = MAINNET.collect_block_spec(block_number);
        let mut processor = ExecutionProcessor::new(
            &mut state,
            None,
            &mut analysis_cache,
            &mut *engine,
            partial_header,
            &block,
            &block_spec,
        );

        let receipt = processor.execute_block_no_post_validation().await?[index].clone();

        let logs: Vec<TxLog> = receipt
            .logs
            .into_iter()
            .enumerate()
            .map(|(i, log)| TxLog {
                log_index: Some(U64::from(i)),
                transaction_index: Some(U64::from(index)),
                transaction_hash: Some(tx_hash),
                block_hash: Some(block_hash),
                block_number: Some(U64::from(block_number.0)),
                address: log.address,
                data: log.data,
                topics: log.topics,
            })
            .collect();

        let to = match msg.action() {
            TransactionAction::Call(to) => Some(to),
            TransactionAction::Create => None,
        };

        let contract_address = match to {
            Some(_) => None,
            None => Some(create_address(msg.sender, msg.nonce())),
        };

        Ok(TxReceipt {
            transaction_hash: tx_hash,
            transaction_index: U64::from(index),
            block_hash,
            block_number: U64::from(block_number.0),
            from: msg.sender,
            to,
            cumulative_gas_used: U64::from(receipt.cumulative_gas_used),
            gas_used: U64::from(header.gas_used),
            contract_address,
            logs,
            logs_bloom: receipt.bloom,
            status: U64::from(receipt.success as u64),
        })
    }

    async fn get_uncle_by_block_hash_and_index(
        &self,
        block_hash: H256,
        index: UncleIndex,
    ) -> RpcResult<jsonrpc::common::Block> {
        let tx = &self.db.begin().await?;

        let block_number = header_number::read(tx, block_hash)
            .await?
            .unwrap_or_default();

        let block_body = block_body::read_without_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let ommer_hash = block_body.ommers[index.as_u64() as usize].hash();

        let ommer_block_number = header_number::read(tx, ommer_hash)
            .await?
            .unwrap_or_default();

        Ok(json_obj::assemble_block(tx, ommer_hash, ommer_block_number, true).await?)
    }

    async fn get_uncle_by_block_number_and_index(
        &self,
        block_number: BlockNumber,
        index: UncleIndex,
    ) -> RpcResult<jsonrpc::common::Block> {
        let tx = &self.db.begin().await?;

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        let block_body = block_body::read_without_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let ommer_hash = block_body.ommers[index.as_u64() as usize].hash();

        let ommer_block_number = header_number::read(tx, ommer_hash)
            .await?
            .unwrap_or_default();

        Ok(json_obj::assemble_block(tx, ommer_hash, ommer_block_number, true).await?)
    }

    async fn get_uncle_count_by_block_hash(&self, block_hash: H256) -> RpcResult<U64> {
        let tx = &self.db.begin().await?;

        let block_number = header_number::read(tx, block_hash)
            .await?
            .unwrap_or_default();

        let block_body = block_body::read_without_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        Ok(U64::from(block_body.ommers.len()))
    }

    async fn get_uncle_count_by_block_number(&self, block_number: BlockNumber) -> RpcResult<U64> {
        let tx = &self.db.begin().await?;

        let block_hash = canonical_hash::read(tx, block_number)
            .await?
            .unwrap_or_default();

        let block_body = block_body::read_without_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        Ok(U64::from(block_body.ommers.len()))
    }
}

pub mod json_obj {
    use super::*;

    pub fn assemble_tx(
        block_hash: H256,
        block_number: BlockNumber,
        msg: &MessageWithSignature,
        index: usize,
    ) -> jsonrpc::common::Tx {
        let v = YParityAndChainId {
            odd_y_parity: msg.signature.odd_y_parity(),
            chain_id: msg.chain_id(),
        };

        let to = match msg.action() {
            TransactionAction::Call(to) => Some(to),
            TransactionAction::Create => None,
        };

        jsonrpc::common::Tx {
            block_hash: Some(block_hash),
            block_number: Some(U64::from(block_number.0)),
            from: msg.recover_sender().unwrap_or_default(),
            gas: U64::from(msg.gas_limit()),
            gas_price: msg.max_fee_per_gas(),
            hash: msg.hash(),
            input: msg.input().clone(),
            nonce: U64::from(msg.nonce()),
            to,
            transaction_index: Some(U64::from(index)),
            value: msg.value(),
            v: U64::from(v.v()),
            r: msg.r(),
            s: msg.s(),
        }
    }

    pub async fn assemble_block<'db, Tx: Transaction<'db>>(
        tx: &Tx,
        block_hash: H256,
        block_number: BlockNumber,
        full_tx_obj: bool,
    ) -> Result<jsonrpc::common::Block, anyhow::Error> {
        let block_body = block_body::read_without_senders(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let msgs = block_body.transactions;

        let txns: Vec<jsonrpc::common::Transaction> = match full_tx_obj {
            true => msgs
                .into_iter()
                .enumerate()
                .map(|(index, msg)| {
                    jsonrpc::common::Transaction::Full(Box::new(json_obj::assemble_tx(
                        block_hash,
                        block_number,
                        &msg,
                        index,
                    )))
                })
                .collect(),
            false => msgs
                .into_iter()
                .map(|msg| jsonrpc::common::Transaction::Partial(msg.hash()))
                .collect(),
        };

        let total_difficulty = td::read(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        let size = rlp::encode(
            &(block_body::read_without_senders(tx, block_hash, block_number)
                .await?
                .unwrap_or_default()),
        )
        .len();

        let ommers = block_body.ommers;
        let ommer_hashes: Vec<H256> = ommers.into_iter().map(|ommer| ommer.hash()).collect();

        let header = header::read(tx, block_hash, block_number)
            .await?
            .unwrap_or_default();

        Ok(jsonrpc::common::Block {
            number: Some(U64::from(header.number.0)),
            hash: Some(block_hash),
            parent_hash: header.parent_hash,
            nonce: Some(header.nonce),
            sha3_uncles: header.ommers_hash,
            logs_bloom: Some(header.logs_bloom),
            transactions_root: header.transactions_root,
            state_root: header.state_root,
            receipts_root: header.receipts_root,
            miner: header.beneficiary,
            difficulty: header.difficulty,
            total_difficulty,
            extra_data: header.extra_data,
            size: U64::from(size),
            gas_limit: U64::from(header.gas_limit),
            gas_used: U64::from(header.gas_used),
            timestamp: U64::from(header.timestamp),
            transactions: txns,
            uncles: ommer_hashes,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();

    let env_filter = if std::env::var(EnvFilter::DEFAULT_ENV)
        .unwrap_or_default()
        .is_empty()
    {
        EnvFilter::new("martinez=info,rpc=info")
    } else {
        EnvFilter::from_default_env()
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(env_filter)
        .init();

    let db = Arc::new(martinez::kv::mdbx::Environment::<mdbx::NoWriteMap>::open_ro(
        mdbx::Environment::new(),
        &opt.datadir,
        martinez::kv::tables::CHAINDATA_TABLES.clone(),
    )?);

    let server = HttpServerBuilder::default().build(opt.listen_address)?;
    let _server_handle = server.start(EthApiServerImpl { db }.into_rpc())?;

    pending().await
}
