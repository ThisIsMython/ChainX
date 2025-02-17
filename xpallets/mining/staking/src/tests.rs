// Copyright 2019-2022 ChainX Project Authors. Licensed under GPL-3.0.

use super::*;
use crate::mock::*;
use frame_support::{assert_err, assert_ok, traits::OnInitialize};

fn t_issue_pcx(to: AccountId, value: Balance) {
    XStaking::mint(&to, value);
}

fn t_register(who: AccountId, initial_bond: Balance) -> DispatchResult {
    let mut referral_id = who.to_string().as_bytes().to_vec();

    if referral_id.len() < 2 {
        referral_id.extend_from_slice(&[0, 0, 0, who as u8]);
    }

    XStaking::register(Origin::signed(who), referral_id, initial_bond)
}

fn t_bond(who: AccountId, target: AccountId, value: Balance) -> DispatchResult {
    XStaking::bond(Origin::signed(who), target, value)
}

fn t_rebond(who: AccountId, from: AccountId, to: AccountId, value: Balance) -> DispatchResult {
    XStaking::rebond(Origin::signed(who), from, to, value)
}

fn t_unbond(who: AccountId, target: AccountId, value: Balance) -> DispatchResult {
    XStaking::unbond(Origin::signed(who), target, value)
}

fn t_withdraw_unbonded(
    who: AccountId,
    target: AccountId,
    unbonded_index: UnbondedIndex,
) -> DispatchResult {
    XStaking::unlock_unbonded_withdrawal(Origin::signed(who), target, unbonded_index)
}

fn t_system_block_number_inc(number: BlockNumber) {
    System::set_block_number(System::block_number() + number);
}

fn t_make_a_validator_candidate(who: AccountId, self_bonded: Balance) {
    t_issue_pcx(who, self_bonded);
    assert_ok!(t_register(who, self_bonded));
}

fn t_start_session(session_index: SessionIndex) {
    assert_eq!(
        <Period as Get<BlockNumber>>::get(),
        1,
        "start_session can only be used with session length 1."
    );
    for i in Session::current_index()..session_index {
        // XStaking::on_finalize(System::block_number());
        System::set_block_number((i + 1).into());
        Timestamp::set_timestamp(System::block_number() * 1000 + INIT_TIMESTAMP);
        Session::on_initialize(System::block_number());
        // XStaking::on_initialize(System::block_number());
    }

    assert_eq!(Session::current_index(), session_index);
}

fn assert_bonded_locks(who: AccountId, value: Balance) {
    assert_eq!(
        *<Locks<Test>>::get(who)
            .entry(LockedType::Bonded)
            .or_default(),
        value
    );
}

fn assert_bonded_withdrawal_locks(who: AccountId, value: Balance) {
    assert_eq!(
        *<Locks<Test>>::get(who)
            .entry(LockedType::BondedWithdrawal)
            .or_default(),
        value
    );
}

#[test]
fn cannot_force_chill_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        t_make_a_validator_candidate(123, 100);
        assert!(XStaking::can_force_chilled());
        assert_ok!(XStaking::chill(Origin::signed(123)));
        assert_ok!(XStaking::chill(Origin::signed(2)));
        assert_ok!(XStaking::chill(Origin::signed(3)));
        assert_ok!(XStaking::chill(Origin::signed(4)));
        assert_err!(
            XStaking::chill(Origin::signed(1)),
            <Error<Test>>::TooFewActiveValidators
        );
        t_make_a_validator_candidate(1234, 100);
        assert_ok!(XStaking::chill(Origin::signed(1)));
    });
}

#[test]
fn bond_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        assert_eq!(
            <ValidatorLedgers<Test>>::get(2),
            ValidatorLedger {
                total_nomination: 20,
                last_total_vote_weight: 0,
                last_total_vote_weight_update: 0,
            }
        );
        assert_eq!(System::block_number(), 1);

        t_system_block_number_inc(1);

        let before_bond = Balances::usable_balance(&1);
        // old_lock 10
        let old_lock = *<Locks<Test>>::get(1).get(&LockedType::Bonded).unwrap();
        // { bonded: 10, unbonded_withdrawal: 0 }
        assert_eq!(
            frame_system::Account::<Test>::get(&1).data,
            pallet_balances::AccountData {
                free: 100,
                reserved: 0,
                misc_frozen: 10,
                fee_frozen: 10
            }
        );
        // { bonded: 20, unbonded_withdrawal: 0 }
        assert_ok!(t_bond(1, 2, 10));
        assert_eq!(
            frame_system::Account::<Test>::get(&1).data,
            pallet_balances::AccountData {
                free: 100,
                reserved: 0,
                misc_frozen: 20,
                fee_frozen: 20
            }
        );

        assert_bonded_locks(1, old_lock + 10);
        assert_eq!(Balances::usable_balance(&1), before_bond - 10);
        assert_eq!(
            <ValidatorLedgers<Test>>::get(2),
            ValidatorLedger {
                total_nomination: 30,
                last_total_vote_weight: 40,
                last_total_vote_weight_update: 2,
            }
        );
        assert_eq!(
            <Nominations<Test>>::get(1, 2),
            NominatorLedger {
                nomination: 10,
                last_vote_weight: 0,
                last_vote_weight_update: 2,
                unbonded_chunks: vec![]
            }
        );

        // { bonded: 12, unbonded_withdrawal: 8 }
        assert_ok!(t_unbond(1, 2, 8));

        // { bonded: 13, unbonded_withdrawal: 8 }
        assert_ok!(t_bond(1, 3, 1));

        assert_bonded_locks(1, 13);
        // https://github.com/chainx-org/ChainX/issues/402
        assert_eq!(
            frame_system::Account::<Test>::get(&1).data,
            pallet_balances::AccountData {
                free: 100,
                reserved: 0,
                misc_frozen: 13 + 8,
                fee_frozen: 13 + 8
            }
        );
    });
}

#[test]
fn total_staking_locked_no_more_than_free_balance_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        assert_ok!(t_bond(1, 2, 80));
        assert_eq!(
            Locks::<Test>::get(&1),
            vec![(LockedType::Bonded, 90)].into_iter().collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&1).data,
            pallet_balances::AccountData {
                free: 100,
                reserved: 0,
                misc_frozen: 90,
                fee_frozen: 90,
            }
        );
        assert_eq!(Balances::usable_balance(&1), 10);

        assert_ok!(t_unbond(1, 2, 80));
        assert_eq!(
            Locks::<Test>::get(&1),
            vec![(LockedType::Bonded, 10), (LockedType::BondedWithdrawal, 80)]
                .into_iter()
                .collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&1).data,
            pallet_balances::AccountData {
                free: 100,
                reserved: 0,
                misc_frozen: 90,
                fee_frozen: 90,
            }
        );
        assert_eq!(Balances::usable_balance(&1), 10);

        // Total locked balance in Staking can not be more than current _usable_ balance.
        assert_err!(t_bond(1, 2, 80), Error::<Test>::InsufficientBalance);
        assert_err!(t_bond(1, 2, 11), Error::<Test>::InsufficientBalance);
        assert_ok!(t_bond(1, 2, 10));
        assert_eq!(
            Locks::<Test>::get(&1),
            vec![(LockedType::Bonded, 20), (LockedType::BondedWithdrawal, 80)]
                .into_iter()
                .collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&1).data,
            pallet_balances::AccountData {
                free: 100,
                reserved: 0,
                misc_frozen: 90 + 10,
                fee_frozen: 90 + 10,
            }
        );
        assert_eq!(Balances::usable_balance(&1), 0);
    });
}

#[test]
fn unbond_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        assert_err!(t_unbond(1, 2, 50), Error::<Test>::InvalidUnbondBalance);

        assert_bonded_locks(1, 10);
        t_system_block_number_inc(1);

        assert_ok!(t_bond(1, 2, 10));
        assert_bonded_locks(1, 10 + 10);

        t_system_block_number_inc(1);

        assert_ok!(t_unbond(1, 2, 5));
        assert_bonded_locks(1, 10 + 10 - 5);
        assert_bonded_withdrawal_locks(1, 5);

        assert_eq!(
            <ValidatorLedgers<Test>>::get(2),
            ValidatorLedger {
                total_nomination: 25,
                last_total_vote_weight: 30 + 20 * 2,
                last_total_vote_weight_update: 3,
            }
        );

        assert_eq!(
            <Nominations<Test>>::get(1, 2),
            NominatorLedger {
                nomination: 5,
                last_vote_weight: 10,
                last_vote_weight_update: 3,
                unbonded_chunks: vec![Unbonded {
                    value: 5,
                    locked_until: 50 * 12 * 24 * 3 + 3
                }],
            }
        );
    });
}

#[test]
fn rebond_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        assert_err!(
            XStaking::unbond(Origin::signed(1), 2, 50),
            Error::<Test>::InvalidUnbondBalance
        );

        // Block 2
        t_system_block_number_inc(1);

        assert_ok!(t_bond(1, 2, 10));

        // Block 3
        t_system_block_number_inc(1);

        assert_ok!(t_rebond(1, 2, 3, 5));

        assert_eq!(
            <ValidatorLedgers<Test>>::get(2),
            ValidatorLedger {
                total_nomination: 25,
                last_total_vote_weight: 10 + 60,
                last_total_vote_weight_update: 3,
            }
        );

        assert_eq!(
            <ValidatorLedgers<Test>>::get(3),
            ValidatorLedger {
                total_nomination: 30 + 5,
                last_total_vote_weight: 30 * 3,
                last_total_vote_weight_update: 3,
            }
        );

        assert_eq!(
            <Nominations<Test>>::get(1, 2),
            NominatorLedger {
                nomination: 5,
                last_vote_weight: 10,
                last_vote_weight_update: 3,
                unbonded_chunks: vec![]
            }
        );

        assert_eq!(
            <Nominations<Test>>::get(1, 3),
            NominatorLedger {
                nomination: 5,
                last_vote_weight: 0,
                last_vote_weight_update: 3,
                unbonded_chunks: vec![]
            }
        );

        assert_eq!(<LastRebondOf<Test>>::get(1), Some(3));

        // Block 4
        t_system_block_number_inc(1);
        assert_err!(t_rebond(1, 2, 3, 3), Error::<Test>::NoMoreRebond);

        // The rebond operation is limited to once per bonding duration.
        assert_ok!(XStaking::set_bonding_duration(Origin::root(), 2));

        t_system_block_number_inc(1);
        assert_err!(t_rebond(1, 2, 3, 3), Error::<Test>::NoMoreRebond);

        t_system_block_number_inc(1);
        assert_ok!(t_rebond(1, 2, 3, 3));
    });
}

#[test]
fn withdraw_unbond_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        t_system_block_number_inc(1);

        let before_bond = Balances::usable_balance(&1);
        assert_ok!(t_bond(1, 2, 10));
        assert_eq!(Balances::usable_balance(&1), before_bond - 10);

        t_system_block_number_inc(1);

        let unbond_value = 10;

        assert_ok!(t_unbond(1, 2, unbond_value));

        assert_eq!(
            <Nominations<Test>>::get(1, 2).unbonded_chunks,
            vec![Unbonded {
                value: unbond_value,
                locked_until: DEFAULT_BONDING_DURATION + 3
            }]
        );

        t_system_block_number_inc(DEFAULT_BONDING_DURATION);
        assert_err!(
            t_withdraw_unbonded(1, 2, 0),
            Error::<Test>::UnbondedWithdrawalNotYetDue
        );

        t_system_block_number_inc(1);

        let before_withdraw_unbonded = Balances::usable_balance(&1);
        assert_ok!(t_withdraw_unbonded(1, 2, 0));
        assert_eq!(
            Balances::usable_balance(&1),
            before_withdraw_unbonded + unbond_value
        );

        assert_bonded_withdrawal_locks(1, 0);

        // Unbond total stakes should work.
        assert_ok!(t_unbond(1, 1, 10));
        t_system_block_number_inc(DEFAULT_VALIDATOR_BONDING_DURATION + 1);
        assert_ok!(t_withdraw_unbonded(1, 1, 0));
        assert_eq!(
            frame_system::Account::<Test>::get(&1).data,
            pallet_balances::AccountData {
                free: 100,
                reserved: 0,
                misc_frozen: 0,
                fee_frozen: 0,
            }
        );
    });
}

// todo! fix
#[ignore]
#[test]
fn regular_staking_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        // Block 1
        t_start_session(1);
        assert_eq!(XStaking::current_era(), Some(0));

        t_make_a_validator_candidate(5, 500);
        t_make_a_validator_candidate(6, 600);
        t_make_a_validator_candidate(7, 700);
        t_make_a_validator_candidate(8, 800);

        t_start_session(2);
        assert_eq!(XStaking::current_era(), Some(1));
        assert_eq!(Session::validators(), vec![4, 3, 2, 1]);

        // TODO: figure out the exact session for validators change.
        // sessions_per_era = 3
        //
        // The new session validators will take effect until new_era's start_session_index + 1.
        //
        // [new_era]current_era:1, start_session_index:3, maybe_new_validators:Some([4, 3, 2, 1, 8, 7])
        // Session Validators: [4, 3, 2, 1]
        //
        // [start_session]:start_session:3, next_active_era:1
        // [new_session]session_index:4, current_era:Some(1)
        // Session Validators: [8, 7, 6, 5, 4, 3]  <--- Session index is still 3
        t_start_session(3);
        assert_eq!(XStaking::current_era(), Some(1));
        assert_eq!(Session::current_index(), 3);
        assert_eq!(Session::validators(), vec![8, 7, 6, 5, 4, 3]);

        t_start_session(4);
        assert_eq!(XStaking::current_era(), Some(1));
        assert_ok!(XStaking::chill(Origin::signed(6)));
        assert_eq!(Session::validators(), vec![8, 7, 6, 5, 4, 3]);

        t_start_session(5);
        assert_eq!(XStaking::current_era(), Some(2));
        assert_ok!(XStaking::chill(Origin::signed(5)));
        assert_eq!(Session::validators(), vec![8, 7, 6, 5, 4, 3]);

        t_start_session(6);
        assert_eq!(XStaking::current_era(), Some(2));
        assert!(XStaking::is_chilled(&5));
        assert!(XStaking::is_chilled(&6));
        assert_eq!(Session::validators(), vec![8, 7, 5, 4, 3, 2]);

        t_start_session(7);
        assert_eq!(XStaking::current_era(), Some(2));
        assert_eq!(Session::validators(), vec![8, 7, 5, 4, 3, 2]);

        t_start_session(8);
        assert_eq!(XStaking::current_era(), Some(3));
        assert_eq!(Session::validators(), vec![8, 7, 5, 4, 3, 2]);

        t_start_session(9);
        assert_eq!(XStaking::current_era(), Some(3));
        assert_eq!(Session::validators(), vec![8, 7, 4, 3, 2, 1]);
    })
}

#[test]
fn staking_reward_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        let t_1 = 67;
        let t_2 = 68;
        let t_3 = 69;

        t_issue_pcx(t_1, 100);
        t_issue_pcx(t_2, 100);
        t_issue_pcx(t_3, 100);

        XStaking::mint(&888, (FIXED_TOTAL / 2) as u128);
        // Total minted per session:
        // 2_500_000_000
        // │
        // ├──> treasury_reward:    300_000_000 12% <--------
        // └──> mining_reward:    2_200_000_000 88%          |
        //    │                                              |
        //    ├──> Staking        1_980_000_000 90%          |
        //    └──> Asset Mining     220_000_000 10% ---------
        //
        // When you start session 1, actually there are 3 session rounds.
        // the session reward has been minted 3 times.
        t_start_session(1);

        let sub_total = 2_500_000_000u128;

        let treasury_reward = sub_total * 12 / 100;
        let mining_reward = sub_total * 88 / 100;

        let staking_mining_reward = mining_reward * 90 / 100;
        let asset_mining_reward = mining_reward * 10 / 100;

        // (1, 10) => 10 / 100
        // (2, 20) => 20 / 100
        // (3, 30) => 30 / 100
        // (4, 40) => 40 / 100
        let total_staked = 100;
        let validators = vec![1, 2, 3, 4];

        let test_validator_reward =
            |validator: AccountId,
             initial_free: Balance,
             staked: Balance,
             session_index: SessionIndex| {
                let val_total_reward = staking_mining_reward * staked / total_staked;
                // 20% -> validator
                // 80% -> validator's reward pot
                assert_eq!(
                    Balances::free_balance(&validator),
                    initial_free + val_total_reward * session_index as u128 / 5
                );
                assert_eq!(
                    Balances::free_balance(
                        &DummyStakingRewardPotAccountDeterminer::reward_pot_account_for(&validator)
                    ),
                    (val_total_reward - val_total_reward / 5) * session_index as u128
                );
            };

        test_validator_reward(1, 100, 10, 1);
        test_validator_reward(2, 200, 20, 1);
        test_validator_reward(3, 300, 30, 1);
        test_validator_reward(4, 400, 40, 1);

        assert_eq!(
            Balances::free_balance(&TREASURY_ACCOUNT),
            (treasury_reward + asset_mining_reward)
        );

        let validators_reward_pot = validators
            .iter()
            .map(DummyStakingRewardPotAccountDeterminer::reward_pot_account_for)
            .collect::<Vec<_>>();

        let issued_manually = 100 * 3;
        let endowed = 100 + 200 + 300 + 400;
        assert_eq!(
            Balances::total_issuance(),
            2_500_000_000u128 + issued_manually + endowed + (FIXED_TOTAL / 2) as u128
        );

        let mut all = vec![TREASURY_ACCOUNT];
        all.extend_from_slice(&[t_1, t_2, t_3]);
        all.extend_from_slice(&validators);
        all.extend_from_slice(&validators_reward_pot);

        let total_issuance = || all.iter().map(Balances::free_balance).sum::<u128>();

        assert_eq!(
            Balances::total_issuance(),
            total_issuance() + (FIXED_TOTAL / 2) as u128
        );

        t_start_session(2);
        assert_eq!(
            Balances::total_issuance(),
            2_500_000_000u128 * 2 + issued_manually + endowed + (FIXED_TOTAL / 2) as u128
        );
    });
}

fn t_reward_pot_balance(validator: AccountId) -> Balance {
    XStaking::free_balance(
        &DummyStakingRewardPotAccountDeterminer::reward_pot_account_for(&validator),
    )
}

#[test]
fn staker_reward_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        let t_1 = 1111;
        let t_2 = 2222;
        let t_3 = 3333;

        t_issue_pcx(t_1, 100);
        t_issue_pcx(t_2, 100);
        t_issue_pcx(t_3, 100);

        XStaking::mint(&888, (FIXED_TOTAL / 2) as u128);

        assert_eq!(
            <ValidatorLedgers<Test>>::get(1),
            ValidatorLedger {
                total_nomination: 10,
                last_total_vote_weight: 0,
                last_total_vote_weight_update: 0,
            }
        );
        assert_ok!(t_bond(t_1, 1, 10));
        assert_eq!(
            <Nominations<Test>>::get(t_1, 1),
            NominatorLedger {
                nomination: 10,
                last_vote_weight: 0,
                last_vote_weight_update: 1,
                unbonded_chunks: vec![]
            }
        );
        assert_eq!(
            <ValidatorLedgers<Test>>::get(1),
            ValidatorLedger {
                total_nomination: 20,
                last_total_vote_weight: 10,
                last_total_vote_weight_update: 1,
            }
        );

        const TOTAL_STAKING_REWARD: Balance = 1_980_000_000;

        let calc_reward_for_pot =
            |validator_votes: Balance, total_staked: Balance, total_reward: Balance| {
                let total_reward_for_validator = validator_votes * total_reward / total_staked;
                let to_validator = total_reward_for_validator / 5;
                total_reward_for_validator - to_validator
            };

        // Block 1
        // total_staked = val(10+10) + val2(20) + val(30) + val(40) = 110
        // reward pot:
        // 1: 1_980_000_000 * 20/110 * 80% = 288_000_000
        // 2: 1_980_000_000 * 20/110 * 80% = 288_000_000
        // 3_1: 1_980_000_000 * 30/110 * 80% = 432_000_000
        // 3_2: 1_980_000_000 * 30/110 * 20% = 108_000_000
        // 4: 1_980_000_000 * 40/110 * 80% = 576_000_000
        t_start_session(1);
        assert_eq!(t_reward_pot_balance(1), 288_000_000);
        assert_eq!(t_reward_pot_balance(2), 288_000_000);
        assert_eq!(t_reward_pot_balance(3), 432_000_000);
        assert_eq!(t_reward_pot_balance(4), 576_000_000);

        assert_eq!(
            <ValidatorLedgers<Test>>::get(2),
            ValidatorLedger {
                total_nomination: 20,
                last_total_vote_weight: 0,
                last_total_vote_weight_update: 0,
            }
        );
        assert_ok!(t_bond(t_2, 2, 20));
        assert_eq!(
            <ValidatorLedgers<Test>>::get(2),
            ValidatorLedger {
                total_nomination: 20 + 20,
                last_total_vote_weight: 20,
                last_total_vote_weight_update: 1,
            }
        );

        // Block 2
        // total_staked = val(10+10) + val2(20+20) + val(30) + val(40) = 130
        // reward pot:
        // There might be a calculation loss using 80% directly, the actual
        // The order is [4, 3, 2, 1] when calculating.
        // calculation is:
        // validator 4: 1_980_000_000 * 40/130 = 609_230_769
        //    |_ validator 4: 609_230_769 * 20%  = 121_846_153
        //    |_ validator 4's reward pot: 576_000_000 + 609_230_769 - 121_846_153 = 1_063_384_616

        t_start_session(2);
        assert_eq!(
            t_reward_pot_balance(4),
            576_000_000 + 609_230_769 - 121_846_153
        );
        assert_eq!(
            t_reward_pot_balance(4),
            576_000_000 + calc_reward_for_pot(40, 130, TOTAL_STAKING_REWARD)
        );
        assert_eq!(t_reward_pot_balance(3), 797_538_462);
        assert_eq!(t_reward_pot_balance(2), 775_384_616);
        assert_eq!(t_reward_pot_balance(1), 531_692_308);
        assert_eq!(t_reward_pot_balance(2), 775_384_616);

        // validator 1: vote weight = 10 + 20 * 1 = 30
        // t_1 vote weight: 10 * 1  = 10
        assert_ok!(XStaking::claim(Origin::signed(t_1), 1));
        // t_1 = reward_pot_balance * 10 / 30
        assert_eq!(XStaking::free_balance(&t_1), 100 + 531_692_308 / 3);

        // validator 2: vote weight = 40 * 1 + 20 = 60
        // t_2 vote weight = 20 * 1 = 20
        assert_ok!(XStaking::claim(Origin::signed(t_2), 2));
        assert_eq!(XStaking::free_balance(&t_2), 100 + 775_384_616 * 20 / 60);

        assert_ok!(XStaking::set_minimum_validator_count(Origin::root(), 3));
        assert_ok!(XStaking::chill(Origin::signed(4)));

        // Block 3
        t_start_session(4);
        // validator 4 is chilled now, not rewards then.
        assert_eq!(
            t_reward_pot_balance(4),
            576_000_000 + calc_reward_for_pot(40, 130, TOTAL_STAKING_REWARD)
        );
    });
}

#[test]
fn slash_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        // todo!("force_new_era_test");
    });
}

#[test]
fn mint_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        assert_eq!(Balances::total_issuance(), 1000);
        let to_mint = 666;
        XStaking::mint(&7777, to_mint);
        assert_eq!(Balances::total_issuance(), 1000 + to_mint);
        assert_eq!(Balances::free_balance(&7777), to_mint);
    });
}

#[test]
fn balances_reserve_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        let who = 7777;
        let to_mint = 10;
        XStaking::mint(&who, to_mint);
        assert_eq!(Balances::free_balance(&who), 10);

        // Bond 6
        XStaking::bond_reserve(&who, 6);
        assert_eq!(Balances::usable_balance(&who), 4);
        assert_eq!(
            XStaking::locks(&who),
            vec![(LockedType::Bonded, 6)].into_iter().collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&who).data,
            pallet_balances::AccountData {
                free: 10,
                reserved: 0,
                misc_frozen: 6,
                fee_frozen: 6
            }
        );
        assert_err!(
            Balances::transfer(Some(who).into(), 6, 1000),
            pallet_balances::Error::<Test, _>::InsufficientBalance
        );

        // Bond 2 extra
        XStaking::bond_reserve(&who, 2);
        assert_eq!(
            XStaking::locks(&who),
            vec![(LockedType::Bonded, 6 + 2)].into_iter().collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&who).data,
            pallet_balances::AccountData {
                free: 10,
                reserved: 0,
                misc_frozen: 8,
                fee_frozen: 8
            }
        );

        // Bond 3 extra
        XStaking::bond_reserve(&who, 3);

        // Unbond 5 now, the frozen balances stay the same,
        // only internal Staking locked state changes.
        assert_ok!(XStaking::unbond_reserve(&who, 5));
        assert_eq!(
            XStaking::locks(&who),
            vec![(LockedType::Bonded, 6), (LockedType::BondedWithdrawal, 5)]
                .into_iter()
                .collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&who).data,
            pallet_balances::AccountData {
                free: 10,
                reserved: 0,
                misc_frozen: 11,
                fee_frozen: 11
            }
        );

        // Unlock unbonded withdrawal 4.
        XStaking::apply_unlock_unbonded_withdrawal(&who, 4);
        assert_eq!(
            XStaking::locks(&who),
            vec![(LockedType::Bonded, 6), (LockedType::BondedWithdrawal, 1)]
                .into_iter()
                .collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&who).data,
            pallet_balances::AccountData {
                free: 10,
                reserved: 0,
                misc_frozen: 11 - 4,
                fee_frozen: 11 - 4
            }
        );

        // Unlock unbonded withdrawal 1.
        XStaking::apply_unlock_unbonded_withdrawal(&who, 1);
        assert_eq!(
            XStaking::locks(&who),
            vec![(LockedType::Bonded, 6)].into_iter().collect()
        );
        assert_eq!(
            frame_system::Account::<Test>::get(&who).data,
            pallet_balances::AccountData {
                free: 10,
                reserved: 0,
                misc_frozen: 11 - 4 - 1,
                fee_frozen: 11 - 4 - 1
            }
        );
    });
}

#[test]
fn referral_id_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        assert_ok!(XStaking::register(
            Origin::signed(111),
            b"referral1".to_vec(),
            0
        ));
        assert_err!(
            XStaking::register(Origin::signed(112), b"referral1".to_vec(), 0),
            Error::<Test>::OccupiedReferralIdentity
        );

        assert_ok!(XStaking::register(
            Origin::signed(112),
            b"referral2".to_vec(),
            0
        ));
    });
}

#[test]
fn migration_session_offset_should_work() {
    ExtBuilder::default().build_and_execute(|| {
        let who = 1;
        let total_issue = <mock::Test as Config>::Currency::total_issuance();
        assert_eq!(total_issue, 1000);
        assert_eq!(XStaking::this_session_reward(), INITIAL_REWARD as u128);

        XStaking::mint(&who, (FIXED_TOTAL / 2 - 1000) as u128);
        assert_eq!(
            XStaking::this_session_reward(),
            (INITIAL_REWARD / 2) as u128
        );

        XStaking::mint(&who, 1000_u128);
        assert_eq!(
            XStaking::this_session_reward(),
            (INITIAL_REWARD / 2) as u128
        );

        XStaking::mint(&who, 350_000_000_000_000_u128);
        assert_eq!(
            XStaking::this_session_reward(),
            (INITIAL_REWARD / 2) as u128
        );

        XStaking::mint(&who, (FIXED_TOTAL / 4 - 350_000_000_000_000 - 1000) as u128);
        assert_eq!(
            XStaking::this_session_reward(),
            (INITIAL_REWARD / 4) as u128
        );

        XStaking::mint(&who, 175_000_000_000_000_u128);
        assert_eq!(
            XStaking::this_session_reward(),
            (INITIAL_REWARD / 4) as u128
        );

        XStaking::mint(&who, (FIXED_TOTAL / 8 - 175_000_000_000_000) as u128);
        assert_eq!(
            XStaking::this_session_reward(),
            (INITIAL_REWARD / 8) as u128
        );
    });
}
