// Copyright 2019-2022 ChainX Project Authors. Licensed under GPL-3.0.

use super::*;

use frame_benchmarking::{account, benchmarks, whitelisted_caller};
use frame_system::RawOrigin;

use crate::Pallet as XAssets;

const ASSET_ID: AssetId = xp_protocol::X_BTC;
const SEED: u32 = 0;

benchmarks! {
    transfer {
        let caller = whitelisted_caller();
        let transfer_amount: BalanceOf<T> = (100000000 * 10_u32).into(); // e.g. 10 btc
        XAssets::<T>::issue(&ASSET_ID, &caller, transfer_amount).unwrap();

        let recipient: T::AccountId = account("recipient", 0, SEED);
        let recipient_lookup: <T::Lookup as StaticLookup>::Source = T::Lookup::unlookup(recipient.clone());
    }: _(RawOrigin::Signed(caller.clone()), recipient_lookup, ASSET_ID, transfer_amount)
    verify {
        assert_eq!(XAssets::<T>::usable_balance(&caller, &ASSET_ID), Zero::zero());
        assert_eq!(XAssets::<T>::usable_balance(&recipient, &ASSET_ID), transfer_amount);
    }

    force_transfer {
        let caller = whitelisted_caller();
        let transfer_amount: BalanceOf<T> = (100000000 * 10_u32).into(); // e.g. 10 btc
        XAssets::<T>::issue(&ASSET_ID, &caller, transfer_amount).unwrap();

        let caller_lookup: <T::Lookup as StaticLookup>::Source = T::Lookup::unlookup(caller.clone());
        let recipient: T::AccountId = account("recipient", 0, SEED);
        let recipient_lookup: <T::Lookup as StaticLookup>::Source = T::Lookup::unlookup(recipient.clone());
    }: _(RawOrigin::Root, caller_lookup, recipient_lookup, ASSET_ID, transfer_amount)
    verify {
        assert_eq!(XAssets::<T>::usable_balance(&caller, &ASSET_ID), Zero::zero());
        assert_eq!(XAssets::<T>::usable_balance(&recipient, &ASSET_ID), transfer_amount);
    }

    set_balance {
        let n in 1 .. AssetType::iter().collect::<Vec<_>>().len() as u32;

        let user: T::AccountId = account("user", 0, SEED);
        let user_lookup: <T::Lookup as StaticLookup>::Source = T::Lookup::unlookup(user.clone());
        let mut balances = BTreeMap::new();
        balances.insert(AssetType::Locked, 1000u32.into());
        balances.insert(AssetType::Locked, 1000u32.into());
        balances.insert(AssetType::Reserved, 1000u32.into());
        balances.insert(AssetType::ReservedWithdrawal, 1000u32.into());
        balances.insert(AssetType::ReservedDexSpot, 1000u32.into());
    }: set_balance(RawOrigin::Root, user_lookup, ASSET_ID, balances.clone())
    verify {
        assert_eq!(XAssets::<T>::asset_balance(&user, &ASSET_ID), balances);
    }

    set_asset_limit {
        let res = AssetRestrictions::DEPOSIT | AssetRestrictions::DESTROY_USABLE;
    }: set_asset_limit(RawOrigin::Root, ASSET_ID, res)
    verify {
        assert_eq!(XAssets::<T>::asset_restrictions_of(&ASSET_ID), res);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{ExtBuilder, Test};
    use frame_support::assert_ok;

    #[test]
    fn test_benchmarks() {
        ExtBuilder::default().build_default().execute_with(|| {
            assert_ok!(Pallet::<Test>::test_benchmark_transfer());
            assert_ok!(Pallet::<Test>::test_benchmark_force_transfer());
            assert_ok!(Pallet::<Test>::test_benchmark_set_balance());
            assert_ok!(Pallet::<Test>::test_benchmark_set_asset_limit());
        });
    }
}
