// Copyright 2019-2022 ChainX Project Authors. Licensed under GPL-3.0.

#![allow(clippy::type_complexity)]
use codec::{Decode, Encode};
use hex_literal::hex;
use sp_core::H160;
use std::{cell::RefCell, collections::BTreeMap, time::Duration};

#[cfg(feature = "std")]
use frame_support::traits::GenesisBuild;
use frame_support::{
    parameter_types, sp_io,
    traits::{LockIdentifier, UnixTime},
    weights::Weight,
    PalletId,
};
use frame_system::EnsureSigned;
use sp_core::{blake2_256, H256};
use sp_keyring::sr25519;
use sp_runtime::{
    testing::Header,
    traits::{BlakeTwo256, IdentityLookup},
    AccountId32, Perbill,
};

use chainx_primitives::AssetId;
use xp_assets_registrar::Chain;
pub use xp_protocol::{X_BTC, X_ETH};
use xpallet_assets::AssetRestrictions;
use xpallet_assets_registrar::AssetInfo;
use xpallet_gateway_common::{trustees, types::TrusteeInfoConfig};

use light_bitcoin::{
    chain::BlockHeader as BtcHeader,
    keys::Network as BtcNetwork,
    primitives::{h256_rev, Compact},
    serialization::{self, Reader},
};
use sp_runtime::traits::AccountIdConversion;
use xpallet_support::traits::MultisigAddressFor;

use crate::{
    self as xpallet_gateway_bitcoin,
    types::{BtcParams, BtcTxVerifier},
    Config, Error,
};

/// The AccountId alias in this test module.
pub(crate) type AccountId = AccountId32;
pub(crate) type BlockNumber = u64;
pub(crate) type Balance = u128;

type UncheckedExtrinsic = frame_system::mocking::MockUncheckedExtrinsic<Test>;
type Block = frame_system::mocking::MockBlock<Test>;

frame_support::construct_runtime!(
    pub enum Test where
        Block = Block,
        NodeBlock = Block,
        UncheckedExtrinsic = UncheckedExtrinsic,
    {
        System: frame_system::{Pallet, Call, Config, Storage, Event<T>},
        Timestamp: pallet_timestamp::{Pallet, Call, Storage},
        Balances: pallet_balances::{Pallet, Call, Storage, Config<T>, Event<T>},
        Elections: pallet_elections_phragmen::{Pallet, Call, Storage, Event<T>, Config<T>},
        Evm: pallet_evm::{Pallet, Call, Storage, Config, Event<T>},
        XAssetsRegistrar: xpallet_assets_registrar::{Pallet, Call, Storage, Event<T>, Config},
        XAssets: xpallet_assets::{Pallet, Call, Storage, Event<T>, Config<T>},
        XAssetsBridge: xpallet_assets_bridge::{Pallet, Call, Storage, Config<T>, Event<T>},
        XGatewayRecords: xpallet_gateway_records::{Pallet, Call, Storage, Event<T>},
        XGatewayCommon: xpallet_gateway_common::{Pallet, Call, Storage, Event<T>, Config<T>},
        XGatewayBitcoin: xpallet_gateway_bitcoin::{Pallet, Call, Storage, Event<T>, Config<T>},
    }
);

parameter_types! {
    pub const BlockHashCount: u64 = 250;
    pub const MaximumBlockWeight: Weight = 1024;
    pub const MaximumBlockLength: u32 = 2 * 1024;
    pub const AvailableBlockRatio: Perbill = Perbill::from_percent(75);
    pub const SS58Prefix: u8 = 42;
}

impl frame_system::Config for Test {
    type BaseCallFilter = frame_support::traits::Everything;
    type BlockWeights = ();
    type BlockLength = ();
    type Origin = Origin;
    type Call = Call;
    type Index = u64;
    type BlockNumber = BlockNumber;
    type Hash = H256;
    type Hashing = BlakeTwo256;
    type AccountId = AccountId;
    type Lookup = IdentityLookup<Self::AccountId>;
    type Header = Header;
    type Event = ();
    type BlockHashCount = BlockHashCount;
    type DbWeight = ();
    type Version = ();
    type PalletInfo = PalletInfo;
    type AccountData = pallet_balances::AccountData<Balance>;
    type OnNewAccount = ();
    type OnKilledAccount = ();
    type SystemWeightInfo = ();
    type SS58Prefix = SS58Prefix;
    type OnSetCode = ();
    type MaxConsumers = frame_support::traits::ConstU32<16>;
}

parameter_types! {
    pub const ExistentialDeposit: u64 = 0;
    pub const MaxReserves: u32 = 50;
}
impl pallet_balances::Config for Test {
    type MaxLocks = ();
    type Balance = Balance;
    type DustRemoval = ();
    type Event = ();
    type ExistentialDeposit = ExistentialDeposit;
    type AccountStore = System;
    type WeightInfo = ();
    type ReserveIdentifier = [u8; 8];
    type MaxReserves = MaxReserves;
}

parameter_types! {
    pub const ElectionsPhragmenPalletId: LockIdentifier = *b"phrelect";
}

frame_support::parameter_types! {
    pub static VotingBondBase: u64 = 2;
    pub static VotingBondFactor: u64 = 0;
    pub static CandidacyBond: u64 = 3;
    pub static DesiredMembers: u32 = 2;
    pub static DesiredRunnersUp: u32 = 0;
    pub static TermDuration: u64 = 5;
    pub static Members: Vec<u64> = vec![];
    pub static Prime: Option<u64> = None;
}

impl pallet_elections_phragmen::Config for Test {
    type Event = ();
    type PalletId = ElectionsPhragmenPalletId;
    type Currency = Balances;
    type ChangeMembers = ();
    type InitializeMembers = ();
    type CurrencyToVote = frame_support::traits::SaturatingCurrencyToVote;
    type CandidacyBond = CandidacyBond;
    type VotingBondBase = VotingBondBase;
    type VotingBondFactor = VotingBondFactor;
    type LoserCandidate = ();
    type KickedMember = ();
    type DesiredMembers = DesiredMembers;
    type DesiredRunnersUp = DesiredRunnersUp;
    type TermDuration = TermDuration;
    type WeightInfo = ();
}

// assets
parameter_types! {
    pub const ChainXAssetId: AssetId = 0;
}

impl xpallet_assets_registrar::Config for Test {
    type Event = ();
    type NativeAssetId = ChainXAssetId;
    type RegistrarHandler = ();
    type WeightInfo = ();
}

parameter_types! {
    pub const TreasuryPalletId: PalletId = PalletId(*b"pcx/trsy");
}

pub struct SimpleTreasuryAccount;
impl xpallet_support::traits::TreasuryAccount<AccountId> for SimpleTreasuryAccount {
    fn treasury_account() -> Option<AccountId> {
        Some(TreasuryPalletId::get().into_account())
    }
}

impl xpallet_assets::Config for Test {
    type Event = ();
    type Currency = Balances;
    type TreasuryAccount = SimpleTreasuryAccount;
    type OnCreatedAccount = frame_system::Provider<Test>;
    type OnAssetChanged = ();
    type WeightInfo = ();
}

// assets
parameter_types! {
    pub const BtcAssetId: AssetId = 1;
}

impl xpallet_gateway_records::Config for Test {
    type Event = ();
    type WeightInfo = ();
}

pub struct MultisigAddr;
impl MultisigAddressFor<AccountId> for MultisigAddr {
    fn calc_multisig(who: &[AccountId], threshold: u16) -> AccountId {
        let entropy = (b"modlpy/utilisuba", who, threshold).using_encoded(blake2_256);
        AccountId::decode(&mut &entropy[..]).unwrap()
    }
}

impl xpallet_gateway_common::Config for Test {
    type Event = ();
    type Validator = ();
    type DetermineMultisigAddress = MultisigAddr;
    type CouncilOrigin = EnsureSigned<AccountId>;
    type Bitcoin = XGatewayBitcoin;
    type BitcoinTrustee = XGatewayBitcoin;
    type BitcoinTrusteeSessionProvider = trustees::bitcoin::BtcTrusteeSessionManager<Test>;
    type BitcoinTotalSupply = XGatewayBitcoin;
    type BitcoinWithdrawalProposal = XGatewayBitcoin;
    type WeightInfo = ();
}

thread_local! {
    pub static NOW: RefCell<Option<Duration>> = RefCell::new(None);
}

pub struct CustomTimestamp;
impl UnixTime for CustomTimestamp {
    fn now() -> Duration {
        NOW.with(|m| {
            m.borrow().unwrap_or_else(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                let start = SystemTime::now();
                start
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
            })
        })
    }
}

parameter_types! {
    pub const MinimumPeriod: u64 = 1000;
}

impl pallet_timestamp::Config for Test {
    type Moment = u64;
    type OnTimestampSet = ();
    type MinimumPeriod = MinimumPeriod;
    type WeightInfo = ();
}

parameter_types! {
    // 0x1111111111111111111111111111111111111111
    pub EvmCaller: H160 = H160::from_slice(&[17u8;20][..]);
    pub ClaimBond: Balance = 100_000_000;
}

impl pallet_evm::Config for Test {
    type FeeCalculator = ();
    type GasWeightMapping = ();
    type CallOrigin = pallet_evm::EnsureAddressRoot<Self::AccountId>;
    type WithdrawOrigin = pallet_evm::EnsureAddressNever<Self::AccountId>;
    type AddressMapping = pallet_evm::HashedAddressMapping<BlakeTwo256>;
    type Currency = Balances;
    type Runner = pallet_evm::runner::stack::Runner<Self>;
    type Event = ();
    type PrecompilesType = ();
    type PrecompilesValue = ();
    type ChainId = ();
    type BlockGasLimit = ();
    type OnChargeTransaction = ();
    type BlockHashMapping = pallet_evm::SubstrateBlockHashMapping<Self>;
    type FindAuthor = ();
    type WeightInfo = ();
}

impl xpallet_assets_bridge::Config for Test {
    type Event = ();
    type EvmCaller = EvmCaller;
    type ClaimBond = ClaimBond;
}

impl Config for Test {
    type Event = ();
    type UnixTime = CustomTimestamp;
    type AccountExtractor = xp_gateway_bitcoin::OpReturnExtractor;
    type TrusteeSessionProvider =
        xpallet_gateway_common::trustees::bitcoin::BtcTrusteeSessionManager<Test>;
    type CouncilOrigin = EnsureSigned<AccountId>;
    type TrusteeInfoUpdate = XGatewayCommon;
    type ReferralBinding = XGatewayCommon;
    type AddressBinding = XGatewayCommon;
    type WeightInfo = ();
}

pub type XGatewayBitcoinErr = Error<Test>;

pub(crate) fn btc() -> (AssetId, AssetInfo, AssetRestrictions) {
    (
        X_BTC,
        AssetInfo::new::<Test>(
            b"X-BTC".to_vec(),
            b"X-BTC".to_vec(),
            Chain::Bitcoin,
            8,
            b"ChainX's cross-chain Bitcoin".to_vec(),
        )
        .unwrap(),
        AssetRestrictions::DESTROY_USABLE,
    )
}

pub struct ExtBuilder;
impl Default for ExtBuilder {
    fn default() -> Self {
        Self
    }
}
impl ExtBuilder {
    pub fn build_mock(
        self,
        btc_genesis: (BtcHeader, u32),
        btc_network: BtcNetwork,
    ) -> sp_io::TestExternalities {
        let mut storage = frame_system::GenesisConfig::default()
            .build_storage::<Test>()
            .unwrap();

        let btc_assets = btc();
        let assets = vec![(btc_assets.0, btc_assets.1, btc_assets.2, true, true)];

        let mut init_assets = vec![];
        let mut assets_restrictions = vec![];
        for (a, b, c, d, e) in assets {
            init_assets.push((a, b, d, e));
            assets_restrictions.push((a, c))
        }

        GenesisBuild::<Test>::assimilate_storage(
            &xpallet_assets_registrar::GenesisConfig {
                assets: init_assets,
            },
            &mut storage,
        )
        .unwrap();

        let _ = xpallet_assets::GenesisConfig::<Test> {
            assets_restrictions,
            endowed: Default::default(),
        }
        .assimilate_storage(&mut storage);

        // let (genesis_info, genesis_hash, network_id) = load_mock_btc_genesis_header_info();
        let genesis_hash = btc_genesis.0.hash();
        let network_id = btc_network;
        let _ = xpallet_gateway_bitcoin::GenesisConfig::<Test> {
            genesis_trustees: vec![],
            genesis_info: btc_genesis,
            genesis_hash,
            network_id,
            params_info: BtcParams::new(
                545259519,            // max_bits
                2 * 60 * 60,          // block_max_future
                2 * 7 * 24 * 60 * 60, // target_timespan_seconds
                10 * 60,              // target_spacing_seconds
                4,                    // retargeting_factor
            ), // retargeting_factor
            verifier: BtcTxVerifier::Recover,
            confirmation_number: 4,
            btc_withdrawal_fee: 0,
            max_withdrawal_count: 100,
        }
        .assimilate_storage(&mut storage);

        sp_io::TestExternalities::new(storage)
    }

    pub fn build(self) -> sp_io::TestExternalities {
        let mut storage = frame_system::GenesisConfig::default()
            .build_storage::<Test>()
            .unwrap();

        let btc_assets = btc();
        let assets = vec![(btc_assets.0, btc_assets.1, btc_assets.2, true, true)];
        // let mut endowed = BTreeMap::new();
        // let endowed_info = vec![(ALICE, 100), (BOB, 200), (CHARLIE, 300), (DAVE, 400)];
        // endowed.insert(btc_assets.0, endowed_info.clone());
        // endowed.insert(eth_assets.0, endowed_info);

        let mut init_assets = vec![];
        let mut assets_restrictions = vec![];
        for (a, b, c, d, e) in assets {
            init_assets.push((a, b, d, e));
            assets_restrictions.push((a, c))
        }

        GenesisBuild::<Test>::assimilate_storage(
            &xpallet_assets_registrar::GenesisConfig {
                assets: init_assets,
            },
            &mut storage,
        )
        .unwrap();

        let _ = xpallet_assets::GenesisConfig::<Test> {
            assets_restrictions,
            endowed: Default::default(),
        }
        .assimilate_storage(&mut storage);

        xpallet_assets_bridge::GenesisConfig::<Test> {
            admin_key: Some(alice()),
        }
        .assimilate_storage(&mut storage)
        .unwrap();

        let info = trustees_info();
        let genesis_trustees = info
            .iter()
            .find_map(|(chain, _, trustee_params)| {
                if *chain == Chain::Bitcoin {
                    Some(
                        trustee_params
                            .iter()
                            .map(|i| (i.0).clone())
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                }
            })
            .unwrap();

        let _ = xpallet_gateway_common::GenesisConfig::<Test> { trustees: info }
            .assimilate_storage(&mut storage);

        let (genesis_info, genesis_hash, network_id) = load_mainnet_btc_genesis_header_info();

        let _ = xpallet_gateway_bitcoin::GenesisConfig::<Test> {
            genesis_trustees,
            genesis_info,
            genesis_hash,
            network_id,
            params_info: BtcParams::new(
                545259519,            // max_bits
                2 * 60 * 60,          // block_max_future
                2 * 7 * 24 * 60 * 60, // target_timespan_seconds
                10 * 60,              // target_spacing_seconds
                4,                    // retargeting_factor
            ), // retargeting_factor
            verifier: BtcTxVerifier::Recover,
            confirmation_number: 4,
            btc_withdrawal_fee: 0,
            max_withdrawal_count: 100,
        }
        .assimilate_storage(&mut storage);

        sp_io::TestExternalities::new(storage)
    }
    pub fn build_and_execute(self, test: impl FnOnce()) {
        let mut ext = self.build();
        ext.execute_with(|| System::set_block_number(1));
        ext.execute_with(test);
    }
}

pub fn alice() -> AccountId32 {
    sr25519::Keyring::Alice.to_account_id()
}
pub fn bob() -> AccountId32 {
    sr25519::Keyring::Bob.to_account_id()
}
pub fn charlie() -> AccountId32 {
    sr25519::Keyring::Charlie.to_account_id()
}
pub fn trustees() -> Vec<(AccountId32, Vec<u8>, Vec<u8>, Vec<u8>)> {
    vec![
        (
            alice(),
            b"Alice".to_vec(),
            hex!("0283f579dd2380bd31355d066086e1b4d46b518987c1f8a64d4c0101560280eae2").to_vec(),
            hex!("0300849497d4f88ebc3e1bc2583677c5abdbd3b63640b3c5c50cd4628a33a2a2ca").to_vec(),
        ),
        (
            bob(),
            b"Bob".to_vec(),
            hex!("027a0868a14bd18e2e45ff3ad960f892df8d0edd1a5685f0a1dc63c7986d4ad55d").to_vec(),
            hex!("032122032ae9656f9a133405ffe02101469a8d62002270a33ceccf0e40dda54d08").to_vec(),
        ),
        (
            charlie(),
            b"Charlie".to_vec(),
            hex!("02c9929543dfa1e0bb84891acd47bfa6546b05e26b7a04af8eb6765fcc969d565f").to_vec(),
            hex!("02b3cc747f572d33f12870fa6866aebbfd2b992ba606b8dc89b676b3697590ad63").to_vec(),
        ),
    ]
}

pub fn load_mainnet_btc_genesis_header_info() -> ((BtcHeader, u32), H256, BtcNetwork) {
    (
        (
            BtcHeader {
                version: 536870912,
                previous_header_hash: h256_rev(
                    "00000010c44946edda38dda2df46c0e56be083e5370508102cb475ff22e21b17",
                ),
                merkle_root_hash: h256_rev(
                    "dbe3a8e027f045d4e50cc12770484b4f4273e248249578942fd77f84e3c3a7b7",
                ),
                time: 1636330862,
                bits: Compact::new(503404827),
                nonce: 2456102,
            },
            63290,
        ),
        h256_rev("0000012504d3007ab7954a6baef767e522bb0d55771acb0fa46f9f4182fd0a0e"),
        BtcNetwork::Testnet,
    )
}

fn trustees_info() -> Vec<(
    Chain,
    TrusteeInfoConfig,
    Vec<(AccountId, Vec<u8>, Vec<u8>, Vec<u8>)>,
)> {
    let btc_trustees = trustees();
    let btc_config = TrusteeInfoConfig {
        min_trustee_count: 3,
        max_trustee_count: 15,
    };
    vec![(Chain::Bitcoin, btc_config, btc_trustees)]
}

pub fn generate_blocks_63290_63310() -> BTreeMap<u32, BtcHeader> {
    let headers = include_str!("./res/headers-63290-63310.json");
    let headers: Vec<(u32, String)> = serde_json::from_str(headers).unwrap();
    headers
        .into_iter()
        .map(|(height, header_hex)| {
            let data = hex::decode(header_hex).unwrap();
            let header = serialization::deserialize(Reader::new(&data)).unwrap();
            (height, header)
        })
        .collect()
}

pub fn generate_blocks_478557_478563() -> (u32, Vec<BtcHeader>, Vec<BtcHeader>) {
    let b0 = BtcHeader {
        version: 0x20000002,
        previous_header_hash: h256_rev(
            "0000000000000000004801aaa0db00c30a6c8d89d16fd30a2115dda5a9fc3469",
        ),
        merkle_root_hash: h256_rev(
            "b2f6c37fb65308f2ff12cfc84e3b4c8d49b02534b86794d7f1dd6d6457327200",
        ),
        time: 1501593084,
        bits: Compact::new(0x18014735),
        nonce: 0x7a511539,
    }; // 478557  btc/bch common use

    let b1: BtcHeader = BtcHeader {
        version: 0x20000002,
        previous_header_hash: h256_rev(
            "000000000000000000eb9bc1f9557dc9e2cfe576f57a52f6be94720b338029e4",
        ),
        merkle_root_hash: h256_rev(
            "5b65144f6518bf4795abd428acd0c3fb2527e4e5c94b0f5a7366f4826001884a",
        ),
        time: 1501593374,
        bits: Compact::new(0x18014735),
        nonce: 0x7559dd16,
    }; //478558  bch forked from here

    let b2: BtcHeader = BtcHeader {
        version: 0x20000002,
        previous_header_hash: h256_rev(
            "0000000000000000011865af4122fe3b144e2cbeea86142e8ff2fb4107352d43",
        ),
        merkle_root_hash: h256_rev(
            "5fa62e1865455037450b7275d838d04f00230556129a4e86621a6bc4ad318c18",
        ),
        time: 1501593780,
        bits: Compact::new(0x18014735),
        nonce: 0xb78dbdba,
    }; // 478559

    let b3: BtcHeader = BtcHeader {
        version: 0x20000002,
        previous_header_hash: h256_rev(
            "00000000000000000019f112ec0a9982926f1258cdcc558dd7c3b7e5dc7fa148",
        ),
        merkle_root_hash: h256_rev(
            "8bd5e10005d8e01aa60278def2025d39b5a441261d934a24bd39e7423866787c",
        ),
        time: 1501594184,
        bits: Compact::new(0x18014735),
        nonce: 0x43628196,
    }; // 478560

    let b4: BtcHeader = BtcHeader {
        version: 0x20000002,
        previous_header_hash: h256_rev(
            "000000000000000000e512213f7303f72c5f7446e6e295f73c28cb024dd79e34",
        ),
        merkle_root_hash: h256_rev(
            "aaa533386910909ed6e6319a3ed2bb86774a8d1d9b373f975d53daad6b12170e",
        ),
        time: 1501594485,
        bits: Compact::new(0x18014735),
        nonce: 0xdabcc394,
    }; // 478561

    let b5: BtcHeader = BtcHeader {
        version: 0x20000002,
        previous_header_hash: h256_rev(
            "0000000000000000008876768068eea31f8f34e2f029765cd2ac998bdc3a2b2d",
        ),
        merkle_root_hash: h256_rev(
            "a51effefcc9eaac767ea211c661e5393d38bf3577b5b7e2d54471098b0ac4e35",
        ),
        time: 1501594711,
        bits: Compact::new(0x18014735),
        nonce: 0xa07f1745,
    }; // 478562

    let b2_fork: BtcHeader = BtcHeader {
        version: 0x20000000,
        previous_header_hash: h256_rev(
            "0000000000000000011865af4122fe3b144e2cbeea86142e8ff2fb4107352d43",
        ),
        merkle_root_hash: h256_rev(
            "c896c91a0be4d3eed5568bab4c3084945e5e06669be38ec06b1c8ca4d84baaab",
        ),
        time: 1501611161,
        bits: Compact::new(0x18014735),
        nonce: 0xe84aca22,
    }; // 478559

    let b3_fork: BtcHeader = BtcHeader {
        version: 0x20000000,
        previous_header_hash: h256_rev(
            "000000000000000000651ef99cb9fcbe0dadde1d424bd9f15ff20136191a5eec",
        ),
        merkle_root_hash: h256_rev(
            "088a7d29c4c6b95a74e362d64a801f492e748369a4fec1ca4e1ab47eefc8af82",
        ),
        time: 1501612386,
        bits: Compact::new(0x18014735),
        nonce: 0xcb72a740,
    }; // 478560
    let b4_fork: BtcHeader = BtcHeader {
        version: 0x20000002,
        previous_header_hash: h256_rev(
            "000000000000000000b15ad892af8f6aca4462d46d0b6e5884cadc033c8f257b",
        ),
        merkle_root_hash: h256_rev(
            "f64de8adf8dac328fb8f1dcb4ba19b6e94de7abc8c4eeaae83df8f62504e8758",
        ),
        time: 1501612639,
        bits: Compact::new(0x18014735),
        nonce: 0x0310f5e2,
    }; // 478561
    let b5_fork: BtcHeader = BtcHeader {
        version: 0x20000000,
        previous_header_hash: h256_rev(
            "00000000000000000013ee8874665f73862a3a0b6a30f895fe34f4c94d3e8a15",
        ),
        merkle_root_hash: h256_rev(
            "a464516af1dab6eadb963b62c5df0e503c8908af503dfff7a169b9d3f9851b11",
        ),
        time: 1501613578,
        bits: Compact::new(0x18014735),
        nonce: 0x0a24f4c4,
    }; // 478562
    let b6_fork: BtcHeader = BtcHeader {
        version: 0x20000000,
        previous_header_hash: h256_rev(
            "0000000000000000005c6e82aa704d326a3a2d6a4aa09f1725f532da8bb8de4d",
        ),
        merkle_root_hash: h256_rev(
            "a27fac4ab26df6e12a33b2bb853140d7e231326ddbc9a1d6611b553b0645a040",
        ),
        time: 1501616264,
        bits: Compact::new(0x18014735),
        nonce: 0x6bd75df1,
    }; // 478563

    (
        478557,
        vec![b0, b1, b2, b3, b4, b5],
        vec![b0, b1, b2_fork, b3_fork, b4_fork, b5_fork, b6_fork],
    )
}
