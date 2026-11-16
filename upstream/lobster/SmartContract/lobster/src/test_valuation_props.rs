// SmartContract/lobster/src/test_valuation_props.rs
#![allow(dead_code)]
#![cfg(test)]

// property tests for the valuation math. the concrete asymmetric-pair
// regression is in test_valuation.rs; this file checks the invariants hold
// as the inputs move (mul_div rounding/overflow, prices, additive tvl, fee cut).

use crate::{Lobster, LobsterClient};
use soroban_sdk::{
    testutils::Address as _, token, Address, Env, IntoVal, Val, Vec, vec,
};

const SCALE: i128 = crate::PRICE_SCALE;

fn create_token<'a>(e: &Env, admin: &Address) -> token::Client<'a> {
    let sac = e.register_stellar_asset_contract_v2(admin.clone());
    token::Client::new(e, &sac.address())
}

fn register_lobster_with_fee(env: &Env, fee_bps: u32) -> LobsterClient<'static> {
    let owner = Address::generate(env);
    let multisig = Address::generate(env);
    let admin = Address::generate(env);
    let token0 = create_token(env, &admin);
    let token1 = create_token(env, &admin);
    let sw = Address::generate(env);
    let args: Vec<Val> = vec![
        env,
        owner.into_val(env),
        multisig.into_val(env),
        token0.address.into_val(env),
        token1.address.into_val(env),
        fee_bps.into_val(env),
        sw.into_val(env),
    ];
    LobsterClient::new(env, &env.register(Lobster, args))
}

fn register_lobster(env: &Env) -> LobsterClient<'static> {
    register_lobster_with_fee(env, 100)
}

// mul_div

#[test]
fn test_mul_div_commutative() {
    let env = Env::default();
    for (x, y, d) in [(6i128, 7i128, 3i128), (1000, SCALE, 7), (123, 456, 11), (SCALE, 3, 1000)] {
        assert_eq!(crate::mul_div(&env, x, y, d), crate::mul_div(&env, y, x, d), "{x}*{y}/{d}");
    }
}

#[test]
fn test_mul_div_identity() {
    let env = Env::default();
    for x in [0i128, 1, SCALE, 10i128.pow(18), i128::MAX] {
        assert_eq!(crate::mul_div(&env, x, 1, 1), x);
    }
}

#[test]
fn test_mul_div_scale_squared() {
    let env = Env::default();
    // SCALE*SCALE = 1e14 fits i128; SCALE*SCALE/SCALE collapses back to SCALE.
    assert_eq!(crate::mul_div(&env, SCALE, SCALE, 1), SCALE * SCALE);
    assert_eq!(crate::mul_div(&env, SCALE, SCALE, SCALE), SCALE);
}

#[test]
fn test_mul_div_i256_max() {
    let env = Env::default();
    // 4 * i128::MAX needs 130 bits; the i256 intermediate holds it, /4 returns MAX.
    assert_eq!(crate::mul_div(&env, i128::MAX, 4, 4), i128::MAX);
    assert_eq!(crate::mul_div(&env, i128::MAX, 1000, 1000), i128::MAX);
}

#[test]
#[should_panic]
fn test_mul_div_quotient_overflow() {
    let env = Env::default();
    // 4*MAX/2 = 2*MAX does not fit i128 on the way back out.
    crate::mul_div(&env, i128::MAX, 4, 2);
}

#[test]
fn test_mul_div_round_trip() {
    let env = Env::default();
    for x in [0i128, 5, 1000, 7_500_000, 10i128.pow(12)] {
        let up = crate::mul_div(&env, x, SCALE, 1);
        assert_eq!(crate::mul_div(&env, up, 1, SCALE), x);
    }
}

#[test]
fn test_mul_div_neg_truncates() {
    let env = Env::default();
    // toward zero, not toward minus infinity: -3, never -4
    assert_eq!(crate::mul_div(&env, -7, 1, 2), -3);
}

#[test]
fn test_mul_div_neg_denom() {
    let env = Env::default();
    assert_eq!(crate::mul_div(&env, 6, 1, -3), -2);
}

#[test]
fn test_mul_div_two_negatives() {
    let env = Env::default();
    assert_eq!(crate::mul_div(&env, -6, -1, 3), 2);
}

#[test]
fn test_mul_div_split_numerator() {
    let env = Env::default();
    let (a, b, k, d) = (100i128, 250i128, SCALE, 3i128);
    let split = crate::mul_div(&env, a, k, d) + crate::mul_div(&env, b, k, d);
    let combined = crate::mul_div(&env, a + b, k, d);
    // floor(ak/d)+floor(bk/d) <= floor((a+b)k/d) <= that + 1
    assert!(combined >= split && combined <= split + 1, "split={split} combined={combined}");
}

// get_prices_from_reserves

#[test]
fn test_price_monotonic() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let mut last = 0i128;
    for r1 in [1i128, 3, 10, 100, 1000] {
        let (p01, _p10) = lobster.get_prices_from_reserves(&1000i128, &r1);
        assert!(p01 > last, "price_0_in_1 not increasing with reserve1: {p01} after {last}");
        last = p01;
    }
}

#[test]
fn test_price_million_to_one() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1i128, &1_000_000i128);
    assert_eq!(p01, 1_000_000 * SCALE);
    assert_eq!(p10, SCALE / 1_000_000);
    assert!(p10 > 0);
}

#[test]
fn test_price_equal_reserves() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    for r in [1i128, 7, 1000, 10i128.pow(6), 10i128.pow(12)] {
        let (p01, p10) = lobster.get_prices_from_reserves(&r, &r);
        assert_eq!(p01, SCALE, "equal reserves {r}");
        assert_eq!(p10, SCALE, "equal reserves {r}");
    }
}

#[test]
fn test_price_reciprocal_ratios() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let tol = SCALE / 1000; // 0.1%
    for (r0, r1) in [(1000i128, 3i128), (3, 1000), (7, 99), (5, 1_000_000), (123456, 789)] {
        let (p01, p10) = lobster.get_prices_from_reserves(&r0, &r1);
        let product = crate::mul_div(&env, p01, p10, SCALE);
        assert!(
            (product - SCALE).abs() <= tol,
            "reciprocal product off for {r0}/{r1}: {product}",
        );
    }
}

#[test]
fn test_price_swap_symmetric() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    for (r0, r1) in [(1000i128, 3i128), (7, 99), (123456, 789), (5, 1_000_000)] {
        let (a01, a10) = lobster.get_prices_from_reserves(&r0, &r1);
        let (b01, b10) = lobster.get_prices_from_reserves(&r1, &r0);
        assert_eq!(a01, b10, "{r0}/{r1}");
        assert_eq!(a10, b01, "{r0}/{r1}");
    }
}

// get_token_tvl_from_reserves

#[test]
fn test_tvl_linear() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&100i128, &100i128, &p01, &p10);
    let (d0, d1) = lobster.get_token_tvl_from_reserves(&200i128, &200i128, &p01, &p10);
    assert_eq!(d0, 2 * tvl0);
    assert_eq!(d1, 2 * tvl1);
}

#[test]
fn test_tvl_additive() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    let (a0, a1) = lobster.get_token_tvl_from_reserves(&40i128, &10i128, &p01, &p10);
    let (b0, b1) = lobster.get_token_tvl_from_reserves(&60i128, &90i128, &p01, &p10);
    let (s0, s1) = lobster.get_token_tvl_from_reserves(&100i128, &100i128, &p01, &p10);
    assert_eq!(s0, a0 + b0);
    assert_eq!(s1, a1 + b1);
}

#[test]
fn test_tvl_empty() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&0i128, &0i128, &p01, &p10);
    assert_eq!(tvl0, 0);
    assert_eq!(tvl1, 0);
}

#[test]
fn test_tvl_monotonic() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    let (base0, base1) = lobster.get_token_tvl_from_reserves(&100i128, &100i128, &p01, &p10);
    let (more0_0, more0_1) = lobster.get_token_tvl_from_reserves(&101i128, &100i128, &p01, &p10);
    let (more1_0, more1_1) = lobster.get_token_tvl_from_reserves(&100i128, &101i128, &p01, &p10);
    assert!(more0_0 > base0 && more0_1 > base1, "adding token0 must raise both TVL views");
    assert!(more1_0 > base0 && more1_1 > base1, "adding token1 must raise both TVL views");
}

#[test]
fn mul_div_survives_i128_intermediate_overflow() {
    let env = Env::default();
    // x*x = 1.96e38 overflows i128 (max ~1.70e38) but the quotient x*x/x = x fits.
    // a plain checked_mul aborts on the intermediate; mul_div carries it in i256.
    let x = 14_000_000_000_000_000_000i128;
    assert_eq!(crate::mul_div(&env, x, x, x), x);
}

#[test]
fn test_tvl_scarce_conversion() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    // reserve0 scarce (3) against reserve1 (1000)
    let (p01, p10) = lobster.get_prices_from_reserves(&3i128, &1000i128);
    // hold only token1
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&0i128, &7i128, &p01, &p10);
    assert_eq!(tvl0, 7 * p10); // token0_tvl = amount1 * price1_in_0
    assert_eq!(tvl1, 7 * SCALE); // token1_tvl = amount1 * SCALE
}

#[test]
fn test_tvl_dust_positive() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    for (r0, r1) in [(3i128, 1000i128), (1, 1_000_000), (7, 99)] {
        let (p01, p10) = lobster.get_prices_from_reserves(&r0, &r1);
        let (tvl0, _tvl1) = lobster.get_token_tvl_from_reserves(&0i128, &1i128, &p01, &p10);
        assert!(tvl0 > 0, "one unit of token1 valued at zero for {r0}/{r1}");
    }
}

// get_fee_cut: the scale rides through both tvls and cancels, so the fee is
// unchanged by it, linear in the bps, and proportional to the held amounts.

#[test]
fn test_fee_doubles_with_bps() {
    let env = Env::default();
    env.mock_all_auths();
    let one = register_lobster_with_fee(&env, 100);
    let two = register_lobster_with_fee(&env, 200);
    let f1 = one.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_100_000i128, &1_000_000i128, &1_000_000i128);
    let f2 = two.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_100_000i128, &1_000_000i128, &1_000_000i128);
    assert_eq!(f1, (909, 909));
    assert_eq!(f2, (1818, 1818));
}

#[test]
fn test_fee_zero_bps() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster_with_fee(&env, 0);
    let f = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_100_000i128, &1_000_000i128, &1_000_000i128);
    assert_eq!(f, (0, 0));
}

#[test]
fn test_fee_proportional() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    // token1 holding is twice token0; its fee must be twice as large
    let (f0, f1) = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_100_000i128, &1_000_000i128, &2_000_000i128);
    assert_eq!(f1, 2 * f0);
}

#[test]
fn test_fee_large_profit() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let init = 10i128.pow(15);
    let cur = 2 * init;
    let (f0, f1) = lobster.get_fee_cut(&init, &init, &cur, &cur, &init, &init);
    // profit0 = cur - init = 1e15; fee_value = 1e13; fee = 1e15 * 1e13 / 2e15 = 5e12
    assert_eq!(f0, 5_000_000_000_000);
    assert_eq!(f1, 5_000_000_000_000);
}

#[test]
fn test_fee_absorbs_huge() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env); // 100 bps
    // profit ~ i128::MAX/2 overflows profit*bps in i128 before the divide. mul_div
    // carries it in i256, so this returns instead of panicking.
    // fee_total = (MAX/2)/100, total_tvl = MAX/2, fee = 10*fee_total/total_tvl -> 0
    let f = lobster.get_fee_cut(&0i128, &0i128, &(i128::MAX / 2), &0i128, &10i128, &10i128);
    assert_eq!(f, (0, 0));
}

#[test]
fn test_fee_single_numeraire() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env); // 100 bps
    // profit is measured in token0 only. a rise in token0-denominated value is
    // charged even if the token1 leg's nominal value fell.
    // profit = 1_100_000 - 1_000_000 = 100_000; fee_value = 1000;
    // fee = 1_000_000 * 1000 / 1_100_000 = 909
    let gain = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &900_000i128, &1_000_000i128, &1_000_000i128);
    assert_eq!(gain, (909, 909));
    // the mirror: token0-denominated value fell, so no fee even though the token1
    // leg rose. an aggregate of two numeraires would charge a fee on this loss.
    let loss = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &900_000i128, &1_100_000i128, &1_000_000i128, &1_000_000i128);
    assert_eq!(loss, (0, 0));
}

#[test]
fn test_fee_three_to_one() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (f0, f1) = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_100_000i128, &1_000_000i128, &3_000_000i128);
    assert_eq!(f1, 3 * f0);
}

#[test]
fn test_fee_asym_scale_invariant() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let unscaled = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_050_000i128, &700_000i128, &1_300_000i128);
    let scaled = lobster.get_fee_cut(
        &(1_000_000i128 * SCALE),
        &(1_000_000i128 * SCALE),
        &(1_100_000i128 * SCALE),
        &(1_050_000i128 * SCALE),
        &700_000i128,
        &1_300_000i128,
    );
    // the scale carries through both TVLs and cancels, up to where each side floors
    assert!((unscaled.0 - scaled.0).abs() <= 1, "leg0 {unscaled:?} vs {scaled:?}");
    assert!((unscaled.1 - scaled.1).abs() <= 1, "leg1 {unscaled:?} vs {scaled:?}");
}

// init tvl below zero makes total_profit run past total_tvl, so the fee would
// bill more than the held position. a withdraw guards this at the call site;
// get_fee_cut guards it itself now so a direct invoke can't over-charge.
#[test]
#[should_panic]
fn test_fee_negative_init_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    // fee at the cap so the panic is the negative-init guard, not the fee cap.
    // control: test_fee_nonneg_init_never_exceeds_held, same path, init >= 0.
    let lobster = register_lobster_with_fee(&env, crate::MAX_FEE_BPS);
    // init0=-9_000_000 against total_tvl 1_000_000 would bill a fee far past the held amount
    lobster.get_fee_cut(&-9_000_000i128, &0i128, &1_000_000i128, &0i128, &1_000_000i128, &0i128);
}

// the control: same magnitudes, init non-negative, no panic and the fee never
// tops the held amount (here the whole position is profit at 100%, so fee == held)
#[test]
fn test_fee_nonneg_init_never_exceeds_held() {
    let env = Env::default();
    env.mock_all_auths();
    // the max fee the constructor allows; the property (fee never exceeds the
    // held amount) must hold at the ceiling. at 2000 bps of a 1_000_000 profit
    // that is 200_000.
    let lobster = register_lobster_with_fee(&env, crate::MAX_FEE_BPS);
    let (f0, _f1) = lobster.get_fee_cut(&0i128, &0i128, &1_000_000i128, &0i128, &1_000_000i128, &0i128);
    assert!(f0 <= 1_000_000, "fee {f0} exceeds held 1_000_000");
    assert_eq!(f0, 200_000);
}

#[test]
fn test_mul_div_sub_one() {
    let env = Env::default();
    assert_eq!(crate::mul_div(&env, 1, 1, 2), 0);
    assert_eq!(crate::mul_div(&env, 2, 3, 7), 0);
}

#[test]
fn test_mul_div_zero_num() {
    let env = Env::default();
    for d in [1i128, 3, SCALE, 10i128.pow(12)] {
        assert_eq!(crate::mul_div(&env, 0, 999, d), 0);
    }
}

#[test]
fn test_price_clean_divisors() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &250i128);
    assert_eq!(p01, SCALE / 4); // reserve1/reserve0 = 1/4
    assert_eq!(p10, 4 * SCALE); // reserve0/reserve1 = 4
}

#[test]
fn test_price_large_unequal() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&10i128.pow(15), &10i128.pow(9));
    assert_eq!(p01, 10); // 1e9*1e7/1e15
    assert_eq!(p10, 10i128.pow(13)); // 1e15*1e7/1e9
}

#[test]
fn test_tvl_asym_hand_value() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &250i128);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&10i128, &20i128, &p01, &p10);
    assert_eq!(tvl0, 10 * SCALE + 20 * (4 * SCALE)); // amount0*S + amount1*price1_in_0
    assert_eq!(tvl1, 20 * SCALE + 10 * (SCALE / 4)); // amount1*S + amount0*price0_in_1
}

// the fee can never exceed the held amount of either token, even at the max bps
// and with asymmetric holdings: fee_value <= current_tvl0, so amt*fee_value/tvl0 <= amt.
#[test]
fn test_fee_never_exceeds_held_asym() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster_with_fee(&env, crate::MAX_FEE_BPS);
    let (f0, f1) = lobster.get_fee_cut(
        &500_000i128, &500_000i128, &800_000i128, &1_234_567i128, &123_456i128, &987_654i128,
    );
    assert!(f0 >= 0 && f1 >= 0);
    assert!(f0 <= 123_456, "f0 {f0} exceeds held 123_456");
    assert!(f1 <= 987_654, "f1 {f1} exceeds held 987_654");
}

// a larger token0-denominated gain must not produce a smaller fee.
#[test]
fn test_fee_monotonic_in_value() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let a = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &0i128, &1_000_000i128, &0i128);
    let b = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_200_000i128, &0i128, &1_000_000i128, &0i128);
    assert!(b.0 >= a.0, "more gain cut a smaller fee: {a:?} then {b:?}");
}
