//! Local Anvil-based testing environment.
//!
//! [`TestExchange`] spins up Anvil instance with collateral token and exchange smart contracts deployed and
//! provides convenience methods for perpetual contracts setup and account creation.
//!
//! [`TestPerp`] then can be used to configure perpetual contracts and post orders, while [`TestAccount`] provides
//! basic information about exchange account.
//!

use std::{sync::Arc, time::Duration};

use alloy::{
    hex::ToHexExt,
    network::Ethereum,
    node_bindings::{Anvil, AnvilInstance},
    primitives::{Address, I256, U256, address, hex},
    providers::{DynProvider, PendingTransactionBuilder, Provider, ProviderBuilder, ext::AnvilApi},
    rpc::client::RpcClient,
};
use dashmap::{DashMap, DashSet};
use fastnum::{UD64, UD128, udec64};

use crate::{
    Chain,
    abi::{dex::Exchange, testing::TestToken},
    error::DexError,
    num, types,
};

const CHAIN_ID: u64 = 1337;
const BLOCK_TIME_SEC: f64 = 0.45;
const POLL_INTERVAL_MS: u64 = 50;

const USD_DECIMALS: u8 = 6;
const FUNDING_INTERVAL: u64 = 8571;

#[derive(Debug)]
pub struct TestExchange {
    pub chain_id: u64,
    pub rpc_url: String,
    pub provider: DynProvider,
    pub exchange: Exchange::ExchangeInstance<DynProvider>,
    pub token: TestToken::TestTokenInstance<DynProvider>,
    pub owner: Address,
    pub owner_pk: String,
    pub admin: Address,
    pub admin_pk: String,
    pub price_admin: Address,
    pub price_admin_pk: String,
    pub collateral_converter: num::Converter,
    perpetual_ids: Arc<DashSet<types::PerpetualId>>,
    account_address: Arc<DashMap<types::AccountId, Address>>,
    anvil: AnvilInstance,
}

#[derive(Debug)]
pub struct TestPerp<'e> {
    pub id: types::PerpetualId,
    pub name: String,
    pub price_converter: num::Converter,
    pub size_converter: num::Converter,
    pub leverage_converter: num::Converter,
    exchange: &'e TestExchange,
}

#[derive(Debug)]
pub struct TestAccount<'e> {
    pub id: types::AccountId,
    pub address: Address,

    exchange: &'e TestExchange,
}

impl TestExchange {
    pub async fn new() -> Self {
        let anvil = Anvil::new()
            .block_time_f64(BLOCK_TIME_SEC)
            .chain_id(CHAIN_ID)
            .args(vec!["--code-size-limit", "131072"])
            .args(vec!["--gas-limit", "200000000"])
            .args(vec!["--base-fee", "100000000000"])
            .args(vec!["--order", "fifo"])
            .args(vec!["--max-persisted-states", "1000"])
            .try_spawn()
            .unwrap();
        let client = RpcClient::builder().http(anvil.endpoint_url());
        client.set_poll_interval(Duration::from_millis(POLL_INTERVAL_MS));
        let provider = DynProvider::new(
            ProviderBuilder::new()
                .wallet(anvil.wallet().unwrap())
                .connect_client(client),
        );
        // Deploy multicall3 contract (see https://github.com/mds1/multicall3?tab=readme-ov-file#new-deployments)
        provider
            .anvil_set_balance(
                address!("0x05f32b3cc3888453ff71b01135b34ff8e41263f2"),
                U256::from(1e18 as u64),
            )
            .await
            .unwrap();
        _ = provider.send_raw_transaction(&hex!("0xf90f538085174876e800830f42408080b90f00608060405234801561001057600080fd5b50610ee0806100206000396000f3fe6080604052600436106100f35760003560e01c80634d2301cc1161008a578063a8b0574e11610059578063a8b0574e1461025a578063bce38bd714610275578063c3077fa914610288578063ee82ac5e1461029b57600080fd5b80634d2301cc146101ec57806372425d9d1461022157806382ad56cb1461023457806386d516e81461024757600080fd5b80633408e470116100c65780633408e47014610191578063399542e9146101a45780633e64a696146101c657806342cbb15c146101d957600080fd5b80630f28c97d146100f8578063174dea711461011a578063252dba421461013a57806327e86d6e1461015b575b600080fd5b34801561010457600080fd5b50425b6040519081526020015b60405180910390f35b61012d610128366004610a85565b6102ba565b6040516101119190610bbe565b61014d610148366004610a85565b6104ef565b604051610111929190610bd8565b34801561016757600080fd5b50437fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff0140610107565b34801561019d57600080fd5b5046610107565b6101b76101b2366004610c60565b610690565b60405161011193929190610cba565b3480156101d257600080fd5b5048610107565b3480156101e557600080fd5b5043610107565b3480156101f857600080fd5b50610107610207366004610ce2565b73ffffffffffffffffffffffffffffffffffffffff163190565b34801561022d57600080fd5b5044610107565b61012d610242366004610a85565b6106ab565b34801561025357600080fd5b5045610107565b34801561026657600080fd5b50604051418152602001610111565b61012d610283366004610c60565b61085a565b6101b7610296366004610a85565b610a1a565b3480156102a757600080fd5b506101076102b6366004610d18565b4090565b60606000828067ffffffffffffffff8111156102d8576102d8610d31565b60405190808252806020026020018201604052801561031e57816020015b6040805180820190915260008152606060208201528152602001906001900390816102f65790505b5092503660005b8281101561047757600085828151811061034157610341610d60565b6020026020010151905087878381811061035d5761035d610d60565b905060200281019061036f9190610d8f565b6040810135958601959093506103886020850185610ce2565b73ffffffffffffffffffffffffffffffffffffffff16816103ac6060870187610dcd565b6040516103ba929190610e32565b60006040518083038185875af1925050503d80600081146103f7576040519150601f19603f3d011682016040523d82523d6000602084013e6103fc565b606091505b50602080850191909152901515808452908501351761046d577f08c379a000000000000000000000000000000000000000000000000000000000600052602060045260176024527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060445260846000fd5b5050600101610325565b508234146104e6576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601a60248201527f4d756c746963616c6c333a2076616c7565206d69736d6174636800000000000060448201526064015b60405180910390fd5b50505092915050565b436060828067ffffffffffffffff81111561050c5761050c610d31565b60405190808252806020026020018201604052801561053f57816020015b606081526020019060019003908161052a5790505b5091503660005b8281101561068657600087878381811061056257610562610d60565b90506020028101906105749190610e42565b92506105836020840184610ce2565b73ffffffffffffffffffffffffffffffffffffffff166105a66020850185610dcd565b6040516105b4929190610e32565b6000604051808303816000865af19150503d80600081146105f1576040519150601f19603f3d011682016040523d82523d6000602084013e6105f6565b606091505b5086848151811061060957610609610d60565b602090810291909101015290508061067d576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601760248201527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060448201526064016104dd565b50600101610546565b5050509250929050565b43804060606106a086868661085a565b905093509350939050565b6060818067ffffffffffffffff8111156106c7576106c7610d31565b60405190808252806020026020018201604052801561070d57816020015b6040805180820190915260008152606060208201528152602001906001900390816106e55790505b5091503660005b828110156104e657600084828151811061073057610730610d60565b6020026020010151905086868381811061074c5761074c610d60565b905060200281019061075e9190610e76565b925061076d6020840184610ce2565b73ffffffffffffffffffffffffffffffffffffffff166107906040850185610dcd565b60405161079e929190610e32565b6000604051808303816000865af19150503d80600081146107db576040519150601f19603f3d011682016040523d82523d6000602084013e6107e0565b606091505b506020808401919091529015158083529084013517610851577f08c379a000000000000000000000000000000000000000000000000000000000600052602060045260176024527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060445260646000fd5b50600101610714565b6060818067ffffffffffffffff81111561087657610876610d31565b6040519080825280602002602001820160405280156108bc57816020015b6040805180820190915260008152606060208201528152602001906001900390816108945790505b5091503660005b82811015610a105760008482815181106108df576108df610d60565b602002602001015190508686838181106108fb576108fb610d60565b905060200281019061090d9190610e42565b925061091c6020840184610ce2565b73ffffffffffffffffffffffffffffffffffffffff1661093f6020850185610dcd565b60405161094d929190610e32565b6000604051808303816000865af19150503d806000811461098a576040519150601f19603f3d011682016040523d82523d6000602084013e61098f565b606091505b506020830152151581528715610a07578051610a07576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152601760248201527f4d756c746963616c6c333a2063616c6c206661696c656400000000000000000060448201526064016104dd565b506001016108c3565b5050509392505050565b6000806060610a2b60018686610690565b919790965090945092505050565b60008083601f840112610a4b57600080fd5b50813567ffffffffffffffff811115610a6357600080fd5b6020830191508360208260051b8501011115610a7e57600080fd5b9250929050565b60008060208385031215610a9857600080fd5b823567ffffffffffffffff811115610aaf57600080fd5b610abb85828601610a39565b90969095509350505050565b6000815180845260005b81811015610aed57602081850181015186830182015201610ad1565b81811115610aff576000602083870101525b50601f017fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe0169290920160200192915050565b600082825180855260208086019550808260051b84010181860160005b84811015610bb1578583037fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe001895281518051151584528401516040858501819052610b9d81860183610ac7565b9a86019a9450505090830190600101610b4f565b5090979650505050505050565b602081526000610bd16020830184610b32565b9392505050565b600060408201848352602060408185015281855180845260608601915060608160051b870101935082870160005b82811015610c52577fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffa0888703018452610c40868351610ac7565b95509284019290840190600101610c06565b509398975050505050505050565b600080600060408486031215610c7557600080fd5b83358015158114610c8557600080fd5b9250602084013567ffffffffffffffff811115610ca157600080fd5b610cad86828701610a39565b9497909650939450505050565b838152826020820152606060408201526000610cd96060830184610b32565b95945050505050565b600060208284031215610cf457600080fd5b813573ffffffffffffffffffffffffffffffffffffffff81168114610bd157600080fd5b600060208284031215610d2a57600080fd5b5035919050565b7f4e487b7100000000000000000000000000000000000000000000000000000000600052604160045260246000fd5b7f4e487b7100000000000000000000000000000000000000000000000000000000600052603260045260246000fd5b600082357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff81833603018112610dc357600080fd5b9190910192915050565b60008083357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe1843603018112610e0257600080fd5b83018035915067ffffffffffffffff821115610e1d57600080fd5b602001915036819003821315610a7e57600080fd5b8183823760009101908152919050565b600082357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffc1833603018112610dc357600080fd5b600082357fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffa1833603018112610dc357600080fdfea2646970667358221220bb2b5c71a328032f97c676ae39a1ec2148d3e5d6f73d95e9b17910152d61f16264736f6c634300080c00331ca0edce47092c0f398cebf3ffc267f05c8e7076e3b89445e0fe50f6332273d4569ba01b0b9d000e19b24c5869b0fc3b22b0d6fa47cd63316875cbbd577d76e6fde086")).await.unwrap();

        let (owner, admin, price_admin) = (
            anvil.addresses()[0],
            anvil.addresses()[1],
            anvil.addresses()[2],
        );

        // Test USD
        let token = TestToken::deploy(
            provider.clone(),
            "Test USD".to_string(),
            "USD".to_string(),
            USD_DECIMALS,
        )
        .await
        .unwrap();

        // Some allocation to owner for the faucet
        token
            .mint(owner, usd(1_000_000_000))
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();

        // Exchange
        let exchange = Exchange::deploy(
            provider.clone(),
            *token.address(),
            U256::from(FUNDING_INTERVAL),
            false,
        )
        .await
        .map_err::<DexError, _>(DexError::from)
        .unwrap();

        // Setup roles
        exchange
            .setAdministrator(admin, true)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        exchange
            .setPriceAdministrator(price_admin, true)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();

        Self {
            chain_id: anvil.chain_id(),
            rpc_url: anvil.endpoint_url().to_string(),
            provider,
            exchange,
            token,
            owner,
            owner_pk: anvil.nth_key(0).unwrap().to_bytes().encode_hex(),
            admin,
            admin_pk: anvil.nth_key(1).unwrap().to_bytes().encode_hex(),
            price_admin,
            price_admin_pk: anvil.nth_key(2).unwrap().to_bytes().encode_hex(),
            collateral_converter: num::Converter::new(USD_DECIMALS),
            perpetual_ids: Arc::new(DashSet::new()),
            account_address: Arc::new(DashMap::new()),
            anvil,
        }
    }

    pub fn chain(&self) -> Chain {
        Chain {
            chain_id: self.chain_id,
            collateral_token: *self.token.address(),
            deployed_at_block: 0,
            exchange: *self.exchange.address(),
            perpetuals: self.perpetual_ids.iter().map(|p| *p).collect(),
        }
    }

    pub async fn account(&self, idx: usize, usd_balance: u64) -> TestAccount<'_> {
        let address = self.anvil.addresses()[idx + 3]; // skipping owner, admin and price admin
        let target_balance = usd(usd_balance);
        let cur_balance = self.token.balanceOf(address).call().await.unwrap();
        if target_balance > cur_balance {
            self.token
                .mint(address, target_balance - cur_balance)
                .send()
                .await
                .map_err::<DexError, _>(DexError::from)
                .unwrap()
                .get_receipt()
                .await
                .unwrap();
        }
        self.token
            .approve(*self.exchange.address(), target_balance)
            .from(address)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        let receipt = self
            .exchange
            .createAccount(target_balance)
            .from(address)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        let log = receipt.decoded_log::<Exchange::AccountCreated>().unwrap();
        self.account_address.insert(log.id.to(), log.account);
        TestAccount {
            id: log.id.to(),
            address: log.account,
            exchange: self,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn perp(
        &self,
        name: &str,
        perp_id: types::PerpetualId,
        base_price: UD64,
        price_decimals: u8,
        size_decimals: u8,
        taker_fee: UD64,
        maker_fee: UD64,
        initial_margin: UD64,
        maintenance_margin: UD64,
    ) -> TestPerp<'_> {
        let price_converter = num::Converter::new(price_decimals);
        let fee_converter = num::Converter::new(5); // Fees are in 1/100K
        let leverage_converter = num::Converter::new(2); // Margin and leverage are in 100th
        self.exchange
            .addContract(
                name.to_string(),
                name.to_string(),
                U256::from(perp_id),
                price_converter.to_unsigned(base_price),
                U256::from(price_decimals),
                U256::from(size_decimals),
                fee_converter.to_unsigned(taker_fee),
                fee_converter.to_unsigned(maker_fee),
                leverage_converter.to_unsigned(initial_margin),
                leverage_converter.to_unsigned(maintenance_margin),
            )
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        // Ignore oracle to eliminate ChainLink dependency
        self.exchange
            .setIgnOracle(U256::from(perp_id), true)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self.perpetual_ids.insert(perp_id);
        TestPerp {
            id: perp_id,
            name: name.to_string(),
            price_converter,
            size_converter: num::Converter::new(size_decimals),
            leverage_converter,
            exchange: self,
        }
    }

    pub async fn btc_perp(&self) -> TestPerp<'_> {
        self.perp(
            "BTC",
            0x10,
            udec64!(5000),
            1,
            5,
            udec64!(0.00035),
            udec64!(0.00010),
            udec64!(10),
            udec64!(20),
        )
        .await
        .with_mark_price(udec64!(100000))
        .await
        .unpause()
        .await
    }

    pub async fn eth_perp(&self) -> TestPerp<'_> {
        self.perp(
            "ETH",
            0x20,
            udec64!(1),
            2,
            3,
            udec64!(0.00035),
            udec64!(0.00010),
            udec64!(10),
            udec64!(20),
        )
        .await
        .with_mark_price(udec64!(4000))
        .await
        .unpause()
        .await
    }

    pub async fn sol_perp(&self) -> TestPerp<'_> {
        self.perp(
            "SOL",
            0x30,
            udec64!(1),
            2,
            3,
            udec64!(0.00035),
            udec64!(0.00010),
            udec64!(10),
            udec64!(20),
        )
        .await
        .with_mark_price(udec64!(200))
        .await
        .unpause()
        .await
    }

    pub async fn trx_perp(&self) -> TestPerp<'_> {
        self.perp(
            "TRX",
            0x40,
            udec64!(1),
            5,
            0,
            udec64!(0.00035),
            udec64!(0.00010),
            udec64!(10),
            udec64!(20),
        )
        .await
        .with_mark_price(udec64!(0.3))
        .await
        .unpause()
        .await
    }
}

impl<'e> TestPerp<'e> {
    pub async fn with_mark_price(self, price: UD64) -> Self {
        self.exchange
            .exchange
            .updateMarkPricePNS(U256::from(self.id), self.price_converter.to_unsigned(price))
            .from(self.exchange.price_admin)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn with_min_post(self, min: U256) -> Self {
        self.exchange
            .exchange
            .setMinPost(min)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn with_min_settle(self, min: U256) -> Self {
        self.exchange
            .exchange
            .setMinSettle(min)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn unpause(self) -> Self {
        self.exchange
            .exchange
            .setContractPaused(U256::from(self.id), false)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn set_mark_price(&self, price: UD64) {
        self.exchange
            .exchange
            .updateMarkPricePNS(U256::from(self.id), self.price_converter.to_unsigned(price))
            .from(self.exchange.price_admin)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
    }

    pub async fn set_funding_rate(&self, price: u32, rate: i32) {
        self.exchange
            .exchange
            .setFundingSum(
                U256::from(self.id),
                I256::try_from(rate).unwrap(),
                price,
                true,
                true,
            )
            .from(self.exchange.anvil.addresses()[2]) // From Price Admin
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
    }

    pub async fn order(
        &self,
        account_id: types::AccountId,
        request: types::OrderRequest,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .execOrder(request.to_order_desc(
                self.price_converter,
                self.size_converter,
                self.leverage_converter,
                Some(self.exchange.collateral_converter),
            ))
            .from(
                *self
                    .exchange
                    .account_address
                    .get(&account_id)
                    .unwrap()
                    .value(),
            )
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
    }

    pub async fn orders(
        &self,
        account_id: types::AccountId,
        requests: Vec<types::OrderRequest>,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .execOpsAndOrders(
                vec![],
                requests
                    .iter()
                    .map(|req| {
                        req.to_order_desc(
                            self.price_converter,
                            self.size_converter,
                            self.leverage_converter,
                            Some(self.exchange.collateral_converter),
                        )
                    })
                    .collect(),
                true,
            )
            .from(
                *self
                    .exchange
                    .account_address
                    .get(&account_id)
                    .unwrap()
                    .value(),
            )
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
    }
}

impl<'e> TestAccount<'e> {
    pub async fn balance(&self) -> UD128 {
        let acc = self
            .exchange
            .exchange
            .getAccountById(U256::from(self.id))
            .call()
            .await
            .unwrap();
        self.exchange
            .collateral_converter
            .from_unsigned(acc.balanceCNS)
    }

    pub async fn locked_balance(&self) -> UD128 {
        let acc = self
            .exchange
            .exchange
            .getAccountById(U256::from(self.id))
            .call()
            .await
            .unwrap();
        self.exchange
            .collateral_converter
            .from_unsigned(acc.lockedBalanceCNS)
    }
}

pub fn scale(amount: u64, decimals: u8) -> U256 {
    U256::from(amount) * U256::from(10).pow(U256::from(decimals))
}

pub fn usd(amount: u64) -> U256 {
    scale(amount, USD_DECIMALS)
}
