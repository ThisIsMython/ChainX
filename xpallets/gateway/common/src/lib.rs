// Copyright 2019-2022 ChainX Project Authors. Licensed under GPL-3.0.

//! this module is for bridge common parts
//! define trait and type for
//! `trustees`, `crosschain binding` and something others

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::new_without_default, clippy::type_complexity)]

#[cfg(any(feature = "runtime-benchmarks", test))]
mod benchmarking;
#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

mod binding;

/// All migrations.
pub mod migrations;

pub mod traits;
pub mod trustees;
pub mod types;
pub mod utils;
pub mod weights;

use frame_support::{
    dispatch::{DispatchError, DispatchResult},
    ensure,
    log::{error, info},
    traits::{ChangeMembers, Currency, ExistenceRequirement, Get},
};
use frame_system::{ensure_root, ensure_signed, pallet_prelude::OriginFor};

use sp_runtime::{
    traits::{CheckedDiv, Saturating, StaticLookup, UniqueSaturatedInto, Zero},
    SaturatedConversion,
};
use sp_std::{collections::btree_map::BTreeMap, convert::TryFrom, prelude::*};

/// ChainX primitives
use chainx_primitives::{AddrStr, AssetId, ChainAddress, Text};
use xp_gateway_common::DstChain;
use xp_protocol::X_BTC;
use xp_runtime::Memo;

/// ChainX pallets
use xpallet_assets::{AssetRestrictions, BalanceOf, Chain, ChainT, WithdrawalLimit};
use xpallet_gateway_records::{Withdrawal, WithdrawalRecordId};
use xpallet_support::traits::{MultisigAddressFor, Validator};

use self::{
    traits::{ProposalProvider, TotalSupply, TrusteeForChain, TrusteeInfoUpdate, TrusteeSession},
    trustees::bitcoin::BtcTrusteeAddrInfo,
    types::{
        GenericTrusteeIntentionProps, GenericTrusteeSessionInfo, RewardInfo, ScriptInfo,
        TrusteeInfoConfig, TrusteeIntentionProps, TrusteeSessionInfo,
    },
};

pub use pallet::*;
pub use weights::WeightInfo;

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::{pallet_prelude::*, transactional};
    use xp_gateway_common::DstChainConfig;

    #[pallet::config]
    pub trait Config:
        frame_system::Config + pallet_elections_phragmen::Config + xpallet_gateway_records::Config
    {
        /// The overarching event type.
        type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;

        /// Handle validator info.
        type Validator: Validator<Self::AccountId>;

        /// Help to calculate multisig.
        type DetermineMultisigAddress: MultisigAddressFor<Self::AccountId>;

        /// A majority of the council can excute some transactions.
        type CouncilOrigin: EnsureOrigin<Self::Origin>;

        /// Get btc chain info.
        type Bitcoin: ChainT<BalanceOf<Self>>;

        // Generate btc trustee session info.
        type BitcoinTrustee: TrusteeForChain<
            Self::AccountId,
            Self::BlockNumber,
            trustees::bitcoin::BtcTrusteeType,
            trustees::bitcoin::BtcTrusteeAddrInfo,
        >;

        /// Get trustee session info.
        type BitcoinTrusteeSessionProvider: TrusteeSession<
            Self::AccountId,
            Self::BlockNumber,
            trustees::bitcoin::BtcTrusteeAddrInfo,
        >;

        /// When the trust changes, the total supply of btc: total issue + pending deposit. Help
        /// to the allocation of btc withdrawal fees.
        type BitcoinTotalSupply: TotalSupply<BalanceOf<Self>>;

        /// Get btc withdrawal proposal.
        type BitcoinWithdrawalProposal: ProposalProvider;

        /// Weight information for extrinsics in this pallet.
        type WeightInfo: WeightInfo;
    }

    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(PhantomData<T>);

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Create a withdrawal.
        /// Withdraws some balances of `asset_id` to address `addr` of target chain.
        ///
        /// WithdrawalRecord State: `Applying`
        ///
        /// NOTE: `ext` is for the compatibility purpose, e.g., EOS requires a memo when doing the transfer.
        #[pallet::weight(<T as Config>::WeightInfo::withdraw())]
        #[transactional]
        pub fn withdraw(
            origin: OriginFor<T>,
            #[pallet::compact] asset_id: AssetId,
            #[pallet::compact] value: BalanceOf<T>,
            addr: AddrStr,
            ext: Memo,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;

            ensure!(
                xpallet_assets::Pallet::<T>::can_do(&asset_id, AssetRestrictions::WITHDRAW),
                xpallet_assets::Error::<T>::ActionNotAllowed,
            );
            Self::verify_withdrawal(asset_id, value, &addr, &ext)?;

            xpallet_gateway_records::Pallet::<T>::withdraw(&who, asset_id, value, addr, ext)?;
            Ok(())
        }

        /// Cancel the withdrawal by the applicant.
        ///
        /// WithdrawalRecord State: `Applying` ==> `NormalCancel`
        #[pallet::weight(<T as Config>::WeightInfo::cancel_withdrawal())]
        #[transactional]
        pub fn cancel_withdrawal(origin: OriginFor<T>, id: WithdrawalRecordId) -> DispatchResult {
            let from = ensure_signed(origin)?;
            xpallet_gateway_records::Pallet::<T>::cancel_withdrawal(id, &from)
        }

        /// Setup the trustee info.
        ///
        /// The hot and cold public keys of the current trustee cannot be replaced at will. If they
        /// are randomly replaced, the hot and cold public keys of the current trustee before the
        /// replacement will be lost, resulting in the inability to reconstruct the `Mast` tree and
        /// generate the corresponding control block.
        ///
        /// There are two solutions:
        /// - the first is to record the hot and cold public keys for each
        /// trustee renewal, and the trustee can update the hot and cold public keys at will.
        /// - The second is to move these trusts into the `lttle_black_house` when it is necessary
        /// to update the hot and cold public keys of trusts, and renew the trustee.
        /// After the renewal of the trustee is completed, the hot and cold public keys can be
        /// updated.
        ///
        /// The second option is currently selected. `The time when the second option
        /// allows the hot and cold public keys to be updated is that the member is not in the
        /// current trustee and is not in a state of renewal of the trustee`.
        /// The advantage of the second scheme is that there is no need to change the storage
        /// structure and record the hot and cold public keys of previous trusts.
        /// The disadvantage is that the update of the hot and cold public keys requires the
        /// participation of the admin account and the user cannot update the hot and cold public
        /// keys at will.
        #[pallet::weight(< T as Config >::WeightInfo::setup_trustee())]
        pub fn setup_trustee(
            origin: OriginFor<T>,
            proxy_account: Option<T::AccountId>,
            chain: Chain,
            about: Text,
            hot_entity: Vec<u8>,
            cold_entity: Vec<u8>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            // make sure this person is a pre-selected trustee
            // or the trustee is in little black house
            ensure!(
                Self::generate_trustee_pool().contains(&who)
                    || Self::little_black_house(chain).contains(&who),
                Error::<T>::NotTrusteePreselectedMember
            );

            ensure!(
                Self::ensure_not_current_trustee(&who) && !Self::trustee_transition_status(chain),
                Error::<T>::ExistCurrentTrustee
            );

            Self::setup_trustee_impl(who, proxy_account, chain, about, hot_entity, cold_entity)
        }

        /// Manual execution of the election by admin.
        #[pallet::weight(0u64)]
        pub fn excute_trustee_election(origin: OriginFor<T>, chain: Chain) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(|o| Self::try_ensure_trustee_admin(o, chain))
                .map(|_| ())
                .or_else(ensure_root)?;

            Self::do_trustee_election(chain)
        }

        /// Force cancel trustee transition
        ///
        /// This is called by the root.
        #[pallet::weight(0u64)]
        pub fn cancel_trustee_election(origin: OriginFor<T>, chain: Chain) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;

            Self::cancel_trustee_transition_impl(chain)?;
            TrusteeTransitionStatus::<T>::insert(chain, false);
            Ok(())
        }

        /// Move a current trustee into a small black room.
        ///
        /// This is to allow for timely replacement in the event of a problem with a particular trustee.
        /// The trustee will be moved into the small black room.
        ///
        /// This is called by the trustee admin and root.
        /// # <weight>
        /// Since this is a root call and will go into trustee election, we assume full block for now.
        /// # </weight>
        #[pallet::weight(0u64)]
        #[transactional]
        pub fn move_trust_into_black_room(
            origin: OriginFor<T>,
            chain: Chain,
            trustees: Option<Vec<T::AccountId>>,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(|o| Self::try_ensure_trustee_admin(o, chain))
                .map(|_| ())
                .or_else(ensure_root)?;

            info!(
                target: "runtime::gateway::common",
                "[move_trust_into_black_room] Try to move a trustee into black room, trustee:{:?}",
                trustees
            );

            if let Some(trustees) = trustees {
                LittleBlackHouse::<T>::mutate(chain, |l| {
                    for trustee in trustees.iter() {
                        l.push(trustee.clone());
                    }
                    l.sort_unstable();
                    l.dedup();
                });
                trustees.into_iter().for_each(|trustee| {
                    if TrusteeSigRecord::<T>::contains_key(chain, &trustee) {
                        TrusteeSigRecord::<T>::mutate(chain, &trustee, |record| *record = 0);
                    }
                });
            }

            Self::do_trustee_election(chain)?;
            Ok(())
        }

        /// Move member out small black room.
        ///
        /// This is called by the trustee admin and root.
        /// # <weight>
        /// Since this is a root call and will go into trustee election, we assume full block for now.
        /// # </weight>
        #[pallet::weight(0u64)]
        pub fn move_trust_out_black_room(
            origin: OriginFor<T>,
            chain: Chain,
            members: Vec<T::AccountId>,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(|o| Self::try_ensure_trustee_admin(o, chain))
                .map(|_| ())
                .or_else(ensure_root)?;

            info!(
                target: "runtime::gateway::common",
                "[move_trust_into_black_room] Try to move a member out black room, member:{:?}",
                members
            );
            members.into_iter().for_each(|member| {
                if Self::little_black_house(chain).contains(&member) {
                    LittleBlackHouse::<T>::mutate(chain, |house| house.retain(|a| *a != member));
                }
            });

            Ok(())
        }

        /// Assign trustee reward
        ///
        /// Any trust can actively call this to receive the
        /// award for the term the trust is in after the change
        /// of term.
        ///
        /// If a trust has not renewed for a long period of time
        /// (no change in council membership or no unusual
        /// circumstances to not renew), but they want to receive
        /// their award early, they can call this through the council.
        #[pallet::weight(< T as Config >::WeightInfo::claim_trustee_reward())]
        #[transactional]
        pub fn claim_trustee_reward(
            origin: OriginFor<T>,
            chain: Chain,
            session_num: i32,
        ) -> DispatchResult {
            let session_num: u32 = if session_num < 0 {
                match session_num {
                    -1i32 => Self::trustee_session_info_len(chain),
                    -2i32 => Self::trustee_session_info_len(chain)
                        .checked_sub(1)
                        .ok_or(Error::<T>::InvalidSessionNum)?,
                    _ => return Err(Error::<T>::InvalidSessionNum.into()),
                }
            } else {
                session_num as u32
            };

            if session_num == Self::trustee_session_info_len(chain) {
                T::CouncilOrigin::ensure_origin(origin)?;
                // update trustee sig record info (update reward weight)
                TrusteeSessionInfoOf::<T>::mutate(chain, session_num, |info| {
                    if let Some(info) = info {
                        info.0.trustee_list.iter_mut().for_each(|trustee| {
                            trustee.1 = Self::trustee_sig_record(chain, &trustee.0);
                        });
                    }
                });
            } else {
                let session_info = T::BitcoinTrusteeSessionProvider::trustee_session(session_num)?;
                let who = ensure_signed(origin)?;
                ensure!(
                    session_info.trustee_list.iter().any(|n| n.0 == who),
                    Error::<T>::InvalidTrusteeHisMember
                );
            }

            Self::apply_claim_trustee_reward(session_num)
        }

        /// Force trustee election
        ///
        /// Mandatory trustee renewal if the current trustee is not doing anything
        ///
        /// This is called by the root.
        #[pallet::weight(< T as Config >::WeightInfo::force_trustee_election())]
        pub fn force_trustee_election(origin: OriginFor<T>, chain: Chain) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;
            Self::update_transition_status(chain, false, None);

            Ok(())
        }

        /// Force update trustee info
        ///
        /// This is called by the root.
        #[pallet::weight(< T as Config >::WeightInfo::force_update_trustee())]
        pub fn force_update_trustee(
            origin: OriginFor<T>,
            who: T::AccountId,
            proxy_account: Option<T::AccountId>,
            chain: Chain,
            about: Text,
            hot_entity: Vec<u8>,
            cold_entity: Vec<u8>,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;

            Self::setup_trustee_impl(who, proxy_account, chain, about, hot_entity, cold_entity)?;
            Ok(())
        }

        /// Set the referral binding of corresponding chain and account.
        #[pallet::weight(< T as Config >::WeightInfo::force_set_referral_binding())]
        pub fn force_set_referral_binding(
            origin: OriginFor<T>,
            chain: Chain,
            who: <T::Lookup as StaticLookup>::Source,
            referral: <T::Lookup as StaticLookup>::Source,
        ) -> DispatchResult {
            ensure_root(origin)?;
            let who = T::Lookup::lookup(who)?;
            let referral = T::Lookup::lookup(referral)?;
            Self::set_referral_binding(chain, who, referral);
            Ok(())
        }

        /// Set the config of trustee information.
        ///
        /// This is a root-only operation.
        #[pallet::weight(< T as Config >::WeightInfo::set_trustee_info_config())]
        pub fn set_trustee_info_config(
            origin: OriginFor<T>,
            chain: Chain,
            config: TrusteeInfoConfig,
        ) -> DispatchResult {
            ensure_root(origin)?;

            TrusteeInfoConfigOf::<T>::insert(chain, config);
            Ok(())
        }

        /// Set trustee's proxy account
        #[pallet::weight(< T as Config >::WeightInfo::set_trustee_proxy())]
        pub fn set_trustee_proxy(
            origin: OriginFor<T>,
            proxy_account: T::AccountId,
            chain: Chain,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;

            ensure!(
                TrusteeIntentionPropertiesOf::<T>::contains_key(&who, chain),
                Error::<T>::NotRegistered
            );

            Self::set_trustee_proxy_impl(&who, proxy_account, chain);
            Ok(())
        }

        /// Set trustee admin multiply
        ///
        /// In order to incentivize trust administrators, a weighted multiplier
        /// for award distribution to trust administrators is set.
        #[pallet::weight(< T as Config >::WeightInfo::set_trustee_admin_multiply())]
        pub fn set_trustee_admin_multiply(
            origin: OriginFor<T>,
            chain: Chain,
            multiply: u64,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;

            TrusteeAdminMultiply::<T>::insert(chain, multiply);
            Ok(())
        }

        /// Set the trustee admin.
        ///
        /// The trustee admin is the account who can change the trustee list.
        #[pallet::weight(< T as Config >::WeightInfo::set_trustee_admin())]
        pub fn set_trustee_admin(
            origin: OriginFor<T>,
            admin: T::AccountId,
            chain: Chain,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;

            TrusteeAdmin::<T>::insert(chain, admin);
            Ok(())
        }

        /// Set dst chain proxy address
        ///
        /// Used to proxy the address of a certain target chain and help
        /// users to deposit and withdraw from that chain.
        ///
        /// In earlier schemes, the proxy address was a multi-signature address to
        /// ensure basic security. Removed after subsequent use of ultra-light node
        /// scheme.
        #[pallet::weight(0u64)]
        pub fn set_dst_chain_proxy_address(
            origin: OriginFor<T>,
            dst_chain: DstChain,
            proxy_account: T::AccountId,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;

            DstChainProxyAddress::<T>::insert(dst_chain, proxy_account);
            Ok(())
        }

        /// Add named dst chain config
        #[pallet::weight(0u64)]
        pub fn register_dst_chain_config(
            origin: OriginFor<T>,
            prefix: Vec<u8>,
            length: u32,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;

            NamedDstChainConfig::<T>::mutate(|configs| {
                configs.push(DstChainConfig::new(&prefix, length))
            });
            Ok(())
        }

        /// Remove named dst chain config
        #[pallet::weight(0u64)]
        pub fn unregister_dst_chain_config(
            origin: OriginFor<T>,
            prefix: Vec<u8>,
            length: u32,
        ) -> DispatchResult {
            T::CouncilOrigin::try_origin(origin)
                .map(|_| ())
                .or_else(ensure_root)?;

            let chain_config = DstChainConfig::new(&prefix, length);
            NamedDstChainConfig::<T>::mutate(|configs| {
                configs.retain(|config| *config != chain_config)
            });
            Ok(())
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub (super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// A (potential) trustee set the required properties. [who, chain, trustee_props]
        SetTrusteeProps(
            T::AccountId,
            Chain,
            GenericTrusteeIntentionProps<T::AccountId>,
        ),
        /// A trustee set proxy account. [who, chain, proxy_account]
        SetTrusteeProxy(T::AccountId, Chain, T::AccountId),
        /// An account set its referral_account of some chain. [who, chain, referral_account]
        ReferralBinded(T::AccountId, Chain, T::AccountId),
        /// The trustee set of a chain was changed. [chain, session_number, session_info, script_info]
        TrusteeSetChanged(
            Chain,
            u32,
            GenericTrusteeSessionInfo<T::AccountId, T::BlockNumber>,
            u32,
        ),
        /// Treasury transfer to trustee. [source, target, chain, session_number, reward_total]
        TransferTrusteeReward(T::AccountId, T::AccountId, Chain, u32, BalanceOf<T>),
        /// Asset reward to trustee multi_account. [target, asset_id, reward_total]
        TransferAssetReward(T::AccountId, AssetId, BalanceOf<T>),
        /// The native asset of trustee multi_account is assigned. [multi_account, session_number, total_reward]
        AllocNativeReward(T::AccountId, u32, BalanceOf<T>),
        /// The not native asset of trustee multi_account is assigned. [multi_account, session_number, asset_id, total_reward]
        AllocNotNativeReward(T::AccountId, u32, AssetId, BalanceOf<T>),
    }

    #[pallet::error]
    pub enum Error<T> {
        /// the value of withdrawal less than than the minimum value
        InvalidWithdrawal,
        /// convert generic data into trustee session info error
        InvalidGenericData,
        /// trustee session info not found
        InvalidTrusteeSession,
        /// exceed the maximum length of the about field of trustess session info
        InvalidAboutLen,
        /// invalid multisig
        InvalidMultisig,
        /// unsupported chain
        NotSupportedChain,
        /// existing duplicate account
        DuplicatedAccountId,
        /// not registered as trustee
        NotRegistered,
        /// just allow trustee preselected members to set their trustee information
        NotTrusteePreselectedMember,
        /// invalid session number
        InvalidSessionNum,
        /// invalid trustee history member
        InvalidTrusteeHisMember,
        /// invalid multi account
        InvalidMultiAccount,
        /// invalid trustee weight
        InvalidTrusteeWeight,
        /// the last trustee transition was not completed.
        LastTransitionNotCompleted,
        /// prevent transition when the withdrawal proposal exists.
        WithdrawalProposalExist,
        /// the trustee members was not enough.
        TrusteeMembersNotEnough,
        /// exist in current trustee
        ExistCurrentTrustee,
    }

    #[pallet::storage]
    #[pallet::getter(fn trustee_multisig_addr)]
    pub(crate) type TrusteeMultiSigAddr<T: Config> =
        StorageMap<_, Twox64Concat, Chain, T::AccountId, OptionQuery>;

    /// Trustee info config of the corresponding chain.
    #[pallet::storage]
    #[pallet::getter(fn trustee_info_config_of)]
    pub(crate) type TrusteeInfoConfigOf<T: Config> =
        StorageMap<_, Twox64Concat, Chain, TrusteeInfoConfig, ValueQuery>;

    /// Next Trustee session info number of the chain.
    ///
    /// Auto generate a new session number (0) when generate new trustee of a chain.
    /// If the trustee of a chain is changed, the corresponding number will increase by 1.
    ///
    /// NOTE: The number can't be modified by users.
    #[pallet::storage]
    #[pallet::getter(fn trustee_session_info_len)]
    pub(crate) type TrusteeSessionInfoLen<T: Config> =
        StorageMap<_, Twox64Concat, Chain, u32, ValueQuery, DefaultForTrusteeSessionInfoLen>;

    #[pallet::type_value]
    pub fn DefaultForTrusteeSessionInfoLen() -> u32 {
        0
    }

    /// Trustee session info of the corresponding chain and number.
    #[pallet::storage]
    #[pallet::getter(fn trustee_session_info_of)]
    pub(crate) type TrusteeSessionInfoOf<T: Config> = StorageDoubleMap<
        _,
        Twox64Concat,
        Chain,
        Twox64Concat,
        u32,
        GenericTrusteeSessionInfo<T::AccountId, T::BlockNumber>,
    >;

    /// Trustee intention properties of the corresponding account and chain.
    #[pallet::storage]
    #[pallet::getter(fn trustee_intention_props_of)]
    pub(crate) type TrusteeIntentionPropertiesOf<T: Config> = StorageDoubleMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        Twox64Concat,
        Chain,
        GenericTrusteeIntentionProps<T::AccountId>,
    >;

    /// The account of the corresponding chain and chain address.
    #[pallet::storage]
    pub(crate) type AddressBindingOf<T: Config> =
        StorageDoubleMap<_, Twox64Concat, Chain, Blake2_128Concat, ChainAddress, T::AccountId>;

    /// The bound address of the corresponding account and chain.
    #[pallet::storage]
    pub(crate) type BoundAddressOf<T: Config> = StorageDoubleMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        Twox64Concat,
        Chain,
        Vec<ChainAddress>,
        ValueQuery,
    >;

    /// Record the source chain address to the destination chain address.
    /// (src_chain, dst_chain, src_address) -> dst_address
    #[pallet::storage]
    pub(crate) type AddressBindingOfDstChain<T: Config> = StorageNMap<
        _,
        (
            NMapKey<Twox64Concat, Chain>,
            NMapKey<Twox64Concat, DstChain>,
            NMapKey<Blake2_128Concat, ChainAddress>,
        ),
        ChainAddress,
    >;

    /// Record all source chain addresses corresponding to the destination chain address
    /// (dst_address, src_address, dst_address) -> src_addresses
    #[pallet::storage]
    pub(crate) type BoundAddressOfDstChain<T: Config> = StorageNMap<
        _,
        (
            NMapKey<Blake2_128Concat, ChainAddress>,
            NMapKey<Twox64Concat, Chain>,
            NMapKey<Twox64Concat, DstChain>,
        ),
        Vec<ChainAddress>,
        ValueQuery,
    >;

    /// Multiple chains of addresses currently exist, so need to determine
    /// which chain to recharge to by default.
    #[pallet::storage]
    pub(crate) type DefaultDstChain<T: Config> =
        StorageMap<_, Blake2_128Concat, ChainAddress, DstChain>;

    /// Named chain config
    ///
    /// For unsupported named chains need to be filtered to store the transactions
    /// in pending deposit.
    #[pallet::storage]
    #[pallet::getter(fn named_dst_chain_config)]
    pub(crate) type NamedDstChainConfig<T: Config> =
        StorageValue<_, Vec<DstChainConfig>, ValueQuery, DefaultForNamedDstChainConfig>;

    #[pallet::type_value]
    pub fn DefaultForNamedDstChainConfig() -> Vec<DstChainConfig> {
        vec![DstChainConfig::new(b"sui", 20)]
    }

    /// Dst chain's cross-chain proxy address
    #[pallet::storage]
    pub(crate) type DstChainProxyAddress<T: Config> =
        StorageMap<_, Twox64Concat, DstChain, T::AccountId>;

    /// The referral account of the corresponding account and chain.
    #[pallet::storage]
    #[pallet::getter(fn referral_binding_of)]
    pub(crate) type ReferralBindingOf<T: Config> =
        StorageDoubleMap<_, Blake2_128Concat, T::AccountId, Twox64Concat, Chain, T::AccountId>;

    /// Each aggregated public key corresponds to a set of trustees used
    /// to confirm a set of trustees for processing withdrawals.
    #[pallet::storage]
    #[pallet::getter(fn agg_pubkey_info)]
    pub(crate) type AggPubkeyInfo<T: Config> = StorageDoubleMap<
        _,
        Twox64Concat,
        Chain,
        Twox64Concat,
        Vec<u8>,
        Vec<T::AccountId>,
        ValueQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn trustee_admin)]
    pub(crate) type TrusteeAdmin<T: Config> =
        StorageMap<_, Twox64Concat, Chain, T::AccountId, OptionQuery>;

    #[pallet::storage]
    #[pallet::getter(fn trustee_admin_multiply)]
    pub(crate) type TrusteeAdminMultiply<T: Config> =
        StorageMap<_, Twox64Concat, Chain, u64, ValueQuery, DefaultForTrusteeAdminMultiply>;

    #[pallet::type_value]
    pub fn DefaultForTrusteeAdminMultiply() -> u64 {
        11
    }

    #[pallet::storage]
    #[pallet::getter(fn trustee_sig_record)]
    pub(crate) type TrusteeSigRecord<T: Config> =
        StorageDoubleMap<_, Twox64Concat, Chain, Twox64Concat, T::AccountId, u64, ValueQuery>;

    /// The status of the of the trustee transition
    #[pallet::storage]
    #[pallet::getter(fn trustee_transition_status)]
    pub(crate) type TrusteeTransitionStatus<T: Config> =
        StorageMap<_, Twox64Concat, Chain, bool, ValueQuery>;

    /// Members not participating in trustee elections.
    ///
    /// The current trustee members did not conduct multiple signings and put the members in the
    /// little black room. Filter out the member in the next trustee election
    #[pallet::storage]
    #[pallet::getter(fn little_black_house)]
    pub(crate) type LittleBlackHouse<T: Config> =
        StorageMap<_, Twox64Concat, Chain, Vec<T::AccountId>, ValueQuery>;

    /// Record the total number of cross-chain assets at the time of each trust exchange
    #[pallet::storage]
    #[pallet::getter(fn pre_total_supply)]
    pub(crate) type PreTotalSupply<T: Config> =
        StorageDoubleMap<_, Twox64Concat, Chain, Twox64Concat, u32, BalanceOf<T>, ValueQuery>;

    #[pallet::genesis_config]
    pub struct GenesisConfig<T: Config> {
        pub trustees: Vec<(
            Chain,
            TrusteeInfoConfig,
            Vec<(T::AccountId, Text, Vec<u8>, Vec<u8>)>,
        )>,
    }

    #[cfg(feature = "std")]
    impl<T: Config> Default for GenesisConfig<T> {
        fn default() -> Self {
            Self {
                trustees: Default::default(),
            }
        }
    }

    #[pallet::genesis_build]
    impl<T: Config> GenesisBuild<T> for GenesisConfig<T> {
        fn build(&self) {
            let extra_genesis_builder: fn(&Self) = |config| {
                for (chain, info_config, trustee_infos) in config.trustees.iter() {
                    let mut trustees = Vec::with_capacity(trustee_infos.len());
                    for (who, about, hot, cold) in trustee_infos.iter() {
                        Pallet::<T>::setup_trustee_impl(
                            who.clone(),
                            None,
                            *chain,
                            about.clone(),
                            hot.clone(),
                            cold.clone(),
                        )
                        .expect("setup trustee can not fail; qed");
                        trustees.push(who.clone());
                    }
                    TrusteeInfoConfigOf::<T>::insert(chain, info_config.clone());
                }
            };
            extra_genesis_builder(self);
        }
    }
}

// Withdraw
impl<T: Config> Pallet<T> {
    pub fn verify_withdrawal(
        asset_id: AssetId,
        value: BalanceOf<T>,
        addr: &[u8],
        ext: &Memo,
    ) -> DispatchResult {
        ext.check_validity()?;

        let chain = xpallet_assets_registrar::Pallet::<T>::chain_of(&asset_id)?;
        match chain {
            Chain::Bitcoin => {
                // bitcoin do not need memo
                T::Bitcoin::check_addr(addr, b"")?;
            }
            _ => return Err(Error::<T>::NotSupportedChain.into()),
        };
        // we could only split withdrawal limit due to a runtime-api would call `withdrawal_limit`
        // to export `WithdrawalLimit` for an asset.
        let limit = Self::withdrawal_limit(&asset_id)?;
        // withdrawal value should larger than minimal_withdrawal, allow equal
        ensure!(
            value >= limit.minimal_withdrawal,
            Error::<T>::InvalidWithdrawal
        );
        Ok(())
    }
}

/// Trustee setup
impl<T: Config> Pallet<T> {
    pub fn setup_trustee_impl(
        who: T::AccountId,
        proxy_account: Option<T::AccountId>,
        chain: Chain,
        about: Text,
        hot_entity: Vec<u8>,
        cold_entity: Vec<u8>,
    ) -> DispatchResult {
        Self::is_valid_about(&about)?;

        let (hot, cold) = match chain {
            Chain::Bitcoin => {
                let hot = T::BitcoinTrustee::check_trustee_entity(&hot_entity)?;
                let cold = T::BitcoinTrustee::check_trustee_entity(&cold_entity)?;
                (hot.into(), cold.into())
            }
            _ => return Err(Error::<T>::NotSupportedChain.into()),
        };
        // Proxy account, the current usage can be used to generate trust multi-signature accounts
        let proxy_account = if let Some(addr) = proxy_account {
            Some(addr)
        } else {
            Some(who.clone())
        };

        let props = GenericTrusteeIntentionProps::<T::AccountId>(TrusteeIntentionProps::<
            T::AccountId,
            Vec<u8>,
        > {
            proxy_account,
            about,
            hot_entity: hot,
            cold_entity: cold,
        });

        if TrusteeIntentionPropertiesOf::<T>::contains_key(&who, chain) {
            TrusteeIntentionPropertiesOf::<T>::mutate(&who, chain, |t| *t = Some(props.clone()));
        } else {
            TrusteeIntentionPropertiesOf::<T>::insert(&who, chain, props.clone());
        }
        Self::deposit_event(Event::<T>::SetTrusteeProps(who, chain, props));
        Ok(())
    }

    pub fn set_trustee_proxy_impl(who: &T::AccountId, proxy_account: T::AccountId, chain: Chain) {
        TrusteeIntentionPropertiesOf::<T>::mutate(who, chain, |t| {
            if let Some(props) = t {
                props.0.proxy_account = Some(proxy_account.clone());
            }
        });
        Self::deposit_event(Event::<T>::SetTrusteeProxy(
            who.clone(),
            chain,
            proxy_account,
        ));
    }

    fn set_referral_binding(chain: Chain, who: T::AccountId, referral: T::AccountId) {
        ReferralBindingOf::<T>::insert(&who, &chain, referral.clone());
        Self::deposit_event(Event::<T>::ReferralBinded(who, chain, referral))
    }

    pub fn ensure_not_current_trustee(who: &T::AccountId) -> bool {
        if let Ok(info) = T::BitcoinTrusteeSessionProvider::current_trustee_session() {
            !info.trustee_list.into_iter().any(|n| &n.0 == who)
        } else {
            true
        }
    }
}

/// Trustee common
impl<T: Config> Pallet<T> {
    pub fn generate_trustee_pool() -> Vec<T::AccountId> {
        let members = {
            let mut members = pallet_elections_phragmen::Pallet::<T>::members();
            members.sort_unstable_by(|a, b| b.stake.cmp(&a.stake));
            members
                .iter()
                .map(|m| m.who.clone())
                .collect::<Vec<T::AccountId>>()
        };
        let runners_up = {
            let mut runners_up = pallet_elections_phragmen::Pallet::<T>::runners_up();
            runners_up.sort_unstable_by(|a, b| b.stake.cmp(&a.stake));
            runners_up
                .iter()
                .map(|m| m.who.clone())
                .collect::<Vec<T::AccountId>>()
        };
        [members, runners_up].concat()
    }
}

/// Trustee transition
impl<T: Config> Pallet<T> {
    // Make sure the hot and cold pubkey are set and do not check the validity of the address
    pub fn ensure_set_address(who: &T::AccountId, chain: Chain) -> bool {
        Self::trustee_intention_props_of(who, chain).is_some()
    }

    pub fn is_valid_about(about: &[u8]) -> DispatchResult {
        ensure!(about.len() <= 128, Error::<T>::InvalidAboutLen);

        xp_runtime::xss_check(about)
    }

    pub fn do_trustee_election(chain: Chain) -> DispatchResult {
        ensure!(
            !Self::trustee_transition_status(chain),
            Error::<T>::LastTransitionNotCompleted
        );

        ensure!(
            T::BitcoinWithdrawalProposal::get_withdrawal_proposal().is_none(),
            Error::<T>::WithdrawalProposalExist,
        );

        // Current trustee list
        let old_trustee_candidate: Vec<T::AccountId> =
            if let Ok(info) = T::BitcoinTrusteeSessionProvider::current_trustee_session() {
                info.trustee_list.into_iter().unzip::<_, _, _, Vec<u64>>().0
            } else {
                vec![]
            };

        let filter_members: Vec<T::AccountId> = Self::little_black_house(chain);

        let all_trustee_pool = Self::generate_trustee_pool();

        let new_trustee_pool: Vec<T::AccountId> = all_trustee_pool
            .iter()
            .filter_map(|who| {
                match filter_members.contains(who) || !Self::ensure_set_address(who, chain) {
                    true => None,
                    false => Some(who.clone()),
                }
            })
            .collect::<Vec<T::AccountId>>();

        let remain_filter_members = filter_members
            .iter()
            .filter_map(|who| match all_trustee_pool.contains(who) {
                true => Some(who.clone()),
                false => None,
            })
            .collect::<Vec<_>>();

        let desired_members =
            (<T as pallet_elections_phragmen::Config>::DesiredMembers::get() - 1) as usize;

        ensure!(
            new_trustee_pool.len() >= desired_members,
            Error::<T>::TrusteeMembersNotEnough
        );

        let new_trustee_candidate = new_trustee_pool[..desired_members].to_vec();
        let mut new_trustee_candidate_sorted = new_trustee_candidate.clone();
        new_trustee_candidate_sorted.sort_unstable();

        let mut old_trustee_candidate_sorted = old_trustee_candidate;
        old_trustee_candidate_sorted.sort_unstable();
        let (incoming, outgoing) =
            <T as pallet_elections_phragmen::Config>::ChangeMembers::compute_members_diff_sorted(
                &old_trustee_candidate_sorted,
                &new_trustee_candidate_sorted,
            );

        ensure!(
            !incoming.is_empty() || !outgoing.is_empty(),
            Error::<T>::TrusteeMembersNotEnough
        );

        Self::transition_trustee_session_impl(chain, new_trustee_candidate)?;
        LittleBlackHouse::<T>::insert(chain, remain_filter_members);
        if Self::trustee_session_info_len(chain) != 1 {
            TrusteeTransitionStatus::<T>::insert(chain, true);
            let total_supply = T::BitcoinTotalSupply::total_supply();
            PreTotalSupply::<T>::insert(
                chain,
                Self::trustee_session_info_len(chain) - 1,
                total_supply,
            );
        }
        Ok(())
    }

    pub fn try_generate_session_info(
        chain: Chain,
        new_trustees: Vec<T::AccountId>,
    ) -> Result<
        (
            GenericTrusteeSessionInfo<T::AccountId, T::BlockNumber>,
            ScriptInfo<T::AccountId>,
        ),
        DispatchError,
    > {
        let config = Self::trustee_info_config_of(chain);
        let has_duplicate =
            (1..new_trustees.len()).any(|i| new_trustees[i..].contains(&new_trustees[i - 1]));
        if has_duplicate {
            error!(
                target: "runtime::gateway::common",
                "[try_generate_session_info] Duplicate account, candidates:{:?}",
                new_trustees
            );
            return Err(Error::<T>::DuplicatedAccountId.into());
        }
        let mut props = Vec::with_capacity(new_trustees.len());
        for accountid in new_trustees.into_iter() {
            let p = Self::trustee_intention_props_of(&accountid, chain).ok_or_else(|| {
                error!(
                    target: "runtime::gateway::common",
                    "[transition_trustee_session] Candidate {:?} has not registered as a trustee",
                    accountid
                );
                Error::<T>::NotRegistered
            })?;
            props.push((accountid, p));
        }
        let info = match chain {
            Chain::Bitcoin => {
                let props = props
                    .into_iter()
                    .map(|(id, prop)| {
                        (
                            id,
                            TrusteeIntentionProps::<T::AccountId, _>::try_from(prop)
                                .expect("must decode succss from storage data"),
                        )
                    })
                    .collect();
                let session_info = T::BitcoinTrustee::generate_trustee_session_info(props, config)?;

                (session_info.0.into(), session_info.1)
            }
            _ => return Err(Error::<T>::NotSupportedChain.into()),
        };
        Ok(info)
    }

    fn alter_trustee_session(
        chain: Chain,
        session_number: u32,
        session_info: &mut (
            GenericTrusteeSessionInfo<T::AccountId, T::BlockNumber>,
            ScriptInfo<T::AccountId>,
        ),
    ) -> DispatchResult {
        let multi_addr = Self::generate_multisig_addr(chain, &session_info.0)?;
        session_info.0 .0.multi_account = Some(multi_addr.clone());

        TrusteeSessionInfoLen::<T>::insert(chain, session_number);
        TrusteeSessionInfoOf::<T>::insert(chain, session_number, session_info.0.clone());
        TrusteeMultiSigAddr::<T>::insert(chain, multi_addr);
        // Remove the information of the previous aggregate public key，Withdrawal is prohibited at this time.
        AggPubkeyInfo::<T>::remove_all(None);
        for index in 0..session_info.1.agg_pubkeys.len() {
            AggPubkeyInfo::<T>::insert(
                chain,
                &session_info.1.agg_pubkeys[index],
                session_info.1.personal_accounts[index].clone(),
            );
        }
        TrusteeAdmin::<T>::remove(chain);

        Self::deposit_event(Event::<T>::TrusteeSetChanged(
            chain,
            session_number,
            session_info.0.clone(),
            session_info.1.agg_pubkeys.len() as u32,
        ));
        Ok(())
    }

    fn transition_trustee_session_impl(
        chain: Chain,
        new_trustees: Vec<T::AccountId>,
    ) -> DispatchResult {
        let session_number = Self::trustee_session_info_len(chain)
            .checked_add(1)
            .unwrap_or(0u32);
        let mut session_info = Self::try_generate_session_info(chain, new_trustees)?;
        Self::alter_trustee_session(chain, session_number, &mut session_info)
    }

    fn cancel_trustee_transition_impl(chain: Chain) -> DispatchResult {
        let session_number = Self::trustee_session_info_len(chain).saturating_sub(1);
        let trustee_info = Self::trustee_session_info_of(chain, session_number)
            .ok_or(Error::<T>::InvalidTrusteeSession)?;

        let trustees = trustee_info
            .0
            .trustee_list
            .clone()
            .into_iter()
            .unzip::<_, _, _, Vec<u64>>()
            .0;

        let mut session_info = Self::try_generate_session_info(chain, trustees)?;
        session_info.0 = trustee_info;

        Self::alter_trustee_session(chain, session_number, &mut session_info)
    }

    pub fn generate_multisig_addr(
        chain: Chain,
        session_info: &GenericTrusteeSessionInfo<T::AccountId, T::BlockNumber>,
    ) -> Result<T::AccountId, DispatchError> {
        // If there is a proxy account, choose a proxy account
        let mut acc_list: Vec<T::AccountId> = vec![];
        for acc in session_info.0.trustee_list.iter() {
            let acc = Self::trustee_intention_props_of(&acc.0, chain)
                .ok_or_else::<DispatchError, _>(|| {
                    error!(
                        target: "runtime::gateway::common",
                        "[generate_multisig_addr] acc {:?} has not in TrusteeIntentionPropertiesOf",
                        acc.0
                    );
                    Error::<T>::NotRegistered.into()
                })?
                .0
                .proxy_account
                .unwrap_or_else(|| acc.0.clone());
            acc_list.push(acc);
        }

        let multi_addr =
            T::DetermineMultisigAddress::calc_multisig(&acc_list, session_info.0.threshold);

        // Each chain must have a distinct multisig address,
        // duplicated multisig address is not allowed.
        let find_duplicated = Self::trustee_multisigs()
            .into_iter()
            .any(|(c, multisig)| multi_addr == multisig && c == chain);
        ensure!(!find_duplicated, Error::<T>::InvalidMultisig);
        Ok(multi_addr)
    }

    pub fn trustee_multisigs() -> BTreeMap<Chain, T::AccountId> {
        TrusteeMultiSigAddr::<T>::iter().collect()
    }
}

/// Trustee rewards
impl<T: Config> Pallet<T> {
    fn compute_reward<Balance>(
        reward: Balance,
        trustee_info: &TrusteeSessionInfo<T::AccountId, T::BlockNumber, BtcTrusteeAddrInfo>,
    ) -> Result<RewardInfo<T::AccountId, Balance>, DispatchError>
    where
        Balance: Saturating + CheckedDiv + Zero + Copy,
        u64: UniqueSaturatedInto<Balance>,
    {
        let sum_weight = trustee_info
            .trustee_list
            .iter()
            .map(|n| n.1)
            .sum::<u64>()
            .saturated_into::<Balance>();

        let trustee_len = trustee_info.trustee_list.len();
        let mut reward_info = RewardInfo { rewards: vec![] };
        let mut acc_balance = Balance::zero();
        for i in 0..trustee_len - 1 {
            let trustee_weight = trustee_info.trustee_list[i].1.saturated_into::<Balance>();
            let amount = reward
                .saturating_mul(trustee_weight)
                .checked_div(&sum_weight)
                .ok_or(Error::<T>::InvalidTrusteeWeight)?;
            reward_info
                .rewards
                .push((trustee_info.trustee_list[i].0.clone(), amount));
            acc_balance = acc_balance.saturating_add(amount);
        }
        let amount = reward.saturating_sub(acc_balance);
        reward_info
            .rewards
            .push((trustee_info.trustee_list[trustee_len - 1].0.clone(), amount));
        Ok(reward_info)
    }

    fn alloc_native_reward(
        from: &T::AccountId,
        trustee_info: &TrusteeSessionInfo<T::AccountId, T::BlockNumber, BtcTrusteeAddrInfo>,
    ) -> Result<BalanceOf<T>, DispatchError> {
        let total_reward = <T as xpallet_assets::Config>::Currency::free_balance(from);
        if total_reward.is_zero() {
            return Ok(BalanceOf::<T>::zero());
        }
        let reward_info = Self::compute_reward(total_reward, trustee_info)?;
        for (acc, amount) in reward_info.rewards.iter() {
            <T as xpallet_assets::Config>::Currency::transfer(
                from,
                acc,
                *amount,
                ExistenceRequirement::AllowDeath,
            )
            .map_err(|e| {
                error!(
                    target: "runtime::gateway::common",
                    "[apply_claim_trustee_reward] error {:?}, sum_balance:{:?}, reward_info:{:?}.",
                    e, total_reward, reward_info.clone()
                );
                e
            })?;
        }
        Ok(total_reward)
    }

    fn alloc_not_native_reward(
        from: &T::AccountId,
        asset_id: AssetId,
        trustee_info: &TrusteeSessionInfo<T::AccountId, T::BlockNumber, BtcTrusteeAddrInfo>,
    ) -> Result<BalanceOf<T>, DispatchError> {
        xpallet_assets::Pallet::<T>::ensure_not_native_asset(&asset_id)?;
        let total_reward = xpallet_assets::Pallet::<T>::usable_balance(from, &asset_id);
        if total_reward.is_zero() {
            return Ok(BalanceOf::<T>::zero());
        }
        let reward_info = Self::compute_reward(total_reward, trustee_info)?;
        for (acc, amount) in reward_info.rewards.iter() {
            xpallet_assets::Pallet::<T>::move_usable_balance(
                &asset_id, from, acc, *amount,
            )
            .map_err(|e| {
                error!(
                    target: "runtime::gateway::common",
                    "[apply_claim_trustee_reward] error {:?}, sum_balance:{:?}, asset_id: {:?},reward_info:{:?}.",
                    e, total_reward, asset_id, reward_info.clone()
                );
                xpallet_assets::Error::<T>::InsufficientBalance
            })?;
        }
        Ok(total_reward)
    }

    pub fn apply_claim_trustee_reward(session_num: u32) -> DispatchResult {
        let session_info = T::BitcoinTrusteeSessionProvider::trustee_session(session_num)?;
        let multi_account = match session_info.multi_account.clone() {
            None => return Err(Error::<T>::InvalidMultiAccount.into()),
            Some(n) => n,
        };
        // alloc native reward
        match Self::alloc_native_reward(&multi_account, &session_info) {
            Ok(total_native_reward) => {
                if !total_native_reward.is_zero() {
                    Self::deposit_event(Event::<T>::AllocNativeReward(
                        multi_account.clone(),
                        session_num,
                        total_native_reward,
                    ));
                }
            }
            Err(e) => return Err(e),
        }
        // alloc btc reward
        match Self::alloc_not_native_reward(&multi_account, X_BTC, &session_info) {
            Ok(total_btc_reward) => {
                if !total_btc_reward.is_zero() {
                    Self::deposit_event(Event::<T>::AllocNotNativeReward(
                        multi_account,
                        session_num,
                        X_BTC,
                        total_btc_reward,
                    ));
                }
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }
}

/// Ensure trustee admin
impl<T: Config> Pallet<T> {
    fn try_ensure_trustee_admin(origin: OriginFor<T>, chain: Chain) -> Result<(), OriginFor<T>> {
        match ensure_signed(origin.clone()) {
            Ok(who) => {
                if Some(who) != Self::trustee_admin(chain) {
                    return Err(origin);
                }
            }
            Err(_) => return Err(origin),
        }

        Ok(())
    }
}

/// Rpc calls
impl<T: Config> Pallet<T> {
    pub fn withdrawal_limit(
        asset_id: &AssetId,
    ) -> Result<WithdrawalLimit<BalanceOf<T>>, DispatchError> {
        let chain = xpallet_assets_registrar::Pallet::<T>::chain_of(asset_id)?;
        match chain {
            Chain::Bitcoin => T::Bitcoin::withdrawal_limit(asset_id),
            _ => Err(Error::<T>::NotSupportedChain.into()),
        }
    }

    pub fn withdrawal_list_with_fee_info(
        asset_id: &AssetId,
    ) -> Result<
        BTreeMap<
            WithdrawalRecordId,
            (
                Withdrawal<T::AccountId, BalanceOf<T>, T::BlockNumber>,
                WithdrawalLimit<BalanceOf<T>>,
            ),
        >,
        DispatchError,
    > {
        let limit = Self::withdrawal_limit(asset_id)?;

        let result: BTreeMap<
            WithdrawalRecordId,
            (
                Withdrawal<T::AccountId, BalanceOf<T>, T::BlockNumber>,
                WithdrawalLimit<BalanceOf<T>>,
            ),
        > = xpallet_gateway_records::Pallet::<T>::pending_withdrawal_set()
            .map(|(id, record)| {
                (
                    id,
                    (
                        Withdrawal::new(
                            record,
                            xpallet_gateway_records::Pallet::<T>::state_of(id).unwrap_or_default(),
                        ),
                        limit.clone(),
                    ),
                )
            })
            .collect();
        Ok(result)
    }
}
