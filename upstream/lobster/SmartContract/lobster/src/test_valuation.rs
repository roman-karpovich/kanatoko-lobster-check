// SmartContract/lobster/src/test_valuation.rs
#![allow(dead_code)]
#![cfg(test)]

// Valuation math: prices from reserves and TVL. These check the asymmetric
// pair case (USDC/XLM-style, reserves of very different magnitude) where an
// integer reserve/reserve price silently truncates to zero.

use crate::{Lobster, LobsterClient};
use soroban_sdk::{
    testutils::Address as _, token, Address, Env, IntoVal, Val, Vec, vec,
};

fn create_token<'a>(e: &Env, admin: &Address) -> token::Client<'a> {
    let sac = e.register_stellar_asset_contract_v2(admin.clone());
    token::Client::new(e, &sac.address())
}

fn register_lobster(env: &Env) -> LobsterClient<'static> {
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
        100u32.into_val(env),
        sw.into_val(env),
    ];
    LobsterClient::new(env, &env.register(Lobster, args))
}

// An asymmetric pair: 1 unit of the scarce token is worth ~333 of the other.
// price_0_in_1 = reserve1/reserve0 = 3/1000, price_1_in_0 = 1000/3 = 333.
#[test]
fn test_asym_price_not_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);

    let (price_0_in_1, price_1_in_0) = lobster.get_prices_from_reserves(&1000i128, &3i128);

    // price_1_in_0 is fine (333). price_0_in_1 is the fractional side; an
    // integer reserve/reserve drops it to 0, which then zeroes token0's
    // contribution to TVL. A real price never rounds a held position to nothing.
    assert!(price_1_in_0 > 0, "price_1_in_0 should be ~333, got {price_1_in_0}");
    assert!(
        price_0_in_1 > 0,
        "price_0_in_1 truncated to {price_0_in_1}: the scarce-token side of an asymmetric pair is valued at zero",
    );
}

// TVL of a position must reflect both tokens. With a truncated price, the
// token held against the truncated side vanishes from the valuation.
#[test]
fn test_tvl_both_legs() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);

    let (price_0_in_1, price_1_in_0) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    // hold 100 of each token
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&100i128, &100i128, &price_0_in_1, &price_1_in_0);

    // tvl0 = amount0 + amount1 * price_1_in_0 ; both sides must contribute
    assert!(tvl0 > 100, "tvl0 ignored the token1 leg: {tvl0}");
    assert!(tvl1 > 100, "tvl1 ignored the token0 leg: {tvl1}");
}

const SCALE: i128 = crate::PRICE_SCALE;

// mul_div: the 256-bit-intermediate helper the whole valuation fix rests on.

#[test]
fn test_mul_div_exact() {
    let env = Env::default();
    assert_eq!(crate::mul_div(&env, 6, 7, 3), 14);
    assert_eq!(crate::mul_div(&env, 1000, SCALE, 1000), SCALE);
    assert_eq!(crate::mul_div(&env, 0, 999, 7), 0);
}

#[test]
fn test_mul_div_floors() {
    let env = Env::default();
    assert_eq!(crate::mul_div(&env, 7, 1, 2), 3); // 3.5 -> 3
    assert_eq!(crate::mul_div(&env, 1000, SCALE, 3), 3_333_333_333); // 1000e7/3
}

#[test]
fn test_mul_div_phantom_overflow() {
    let env = Env::default();
    let big = 10i128.pow(30); // big*big = 1e60 > i128, but (big*big)/big = 1e30 fits
    assert_eq!(crate::mul_div(&env, big, big, big), big);
}

#[test]
#[should_panic]
fn test_mul_div_overflow_panics() {
    let env = Env::default();
    let big = 10i128.pow(30);
    crate::mul_div(&env, big, big, 1); // 1e60 does not fit i128
}

#[test]
#[should_panic]
fn test_mul_div_zero_denom() {
    let env = Env::default();
    crate::mul_div(&env, 5, 5, 0);
}

// get_prices_from_reserves

#[test]
fn test_price_symmetric_unit() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &1000i128);
    assert_eq!(p01, SCALE);
    assert_eq!(p10, SCALE);
}

#[test]
fn test_price_reciprocal() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    assert_eq!(p01, 30_000); // 3e7/1000
    assert_eq!(p10, 3_333_333_333); // 1000e7/3 floored
    // p01 * p10 / SCALE ~= SCALE (reciprocal), within floor error
    let product = crate::mul_div(&env, p01, p10, SCALE);
    assert!(product > SCALE - SCALE / 1000 && product < SCALE + SCALE / 1000);
}

#[test]
fn test_price_both_directions() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    // 1000/3 and 3/1000 must mirror each other
    let (a01, a10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    let (b01, b10) = lobster.get_prices_from_reserves(&3i128, &1000i128);
    assert_eq!(a01, b10);
    assert_eq!(a10, b01);
}

#[test]
fn test_price_range_no_truncate() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    for (r0, r1) in [(1000i128, 1i128), (1i128, 1000i128), (7i128, 99i128), (123456i128, 789i128), (5i128, 1_000_000i128)] {
        let (p01, p10) = lobster.get_prices_from_reserves(&r0, &r1);
        assert!(p01 > 0, "p01 truncated for {r0}/{r1}");
        assert!(p10 > 0, "p10 truncated for {r0}/{r1}");
    }
}

#[test]
fn test_price_usdc_xlm() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    // 100k USDC against 400k XLM (XLM ~ 0.25 USDC), 7-decimal stroops
    let usdc = 100_000i128 * SCALE;
    let xlm = 400_000i128 * SCALE;
    let (p_usdc_in_xlm, p_xlm_in_usdc) = lobster.get_prices_from_reserves(&usdc, &xlm);
    // 1 USDC ~ 4 XLM, 1 XLM ~ 0.25 USDC
    assert_eq!(p_usdc_in_xlm, 4 * SCALE);
    assert_eq!(p_xlm_in_usdc, SCALE / 4);
}

#[test]
fn test_price_big_reserves() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let big = 10i128.pow(15);
    let (p01, p10) = lobster.get_prices_from_reserves(&big, &big);
    assert_eq!(p01, SCALE);
    assert_eq!(p10, SCALE);
}

#[test]
#[should_panic]
fn test_price_zero_reserve0() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    lobster.get_prices_from_reserves(&0i128, &1000i128);
}

#[test]
#[should_panic]
fn test_price_zero_reserve1() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    lobster.get_prices_from_reserves(&1000i128, &0i128);
}

// get_token_tvl_from_reserves

#[test]
fn test_tvl_symmetric() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &1000i128);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&100i128, &100i128, &p01, &p10);
    // 100*SCALE + 100*SCALE on each side
    assert_eq!(tvl0, 200 * SCALE);
    assert_eq!(tvl1, 200 * SCALE);
}

#[test]
fn test_tvl_hand_value() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&100i128, &100i128, &p01, &p10);
    // token0_tvl = amount0*SCALE + amount1*p10 ; token1_tvl = amount1*SCALE + amount0*p01
    assert_eq!(tvl0, 100 * SCALE + 100 * 3_333_333_333);
    assert_eq!(tvl1, 100 * SCALE + 100 * 30_000);
}

#[test]
fn test_tvl_dust_scarce() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    // scarce token0 (reserve 3 vs 1000): holding 1 of token0 must still be worth
    // something in token1 terms
    let (p01, p10) = lobster.get_prices_from_reserves(&3i128, &1000i128);
    let (_tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&1i128, &0i128, &p01, &p10);
    assert!(tvl1 > 0, "a unit of the scarce token valued at zero: {tvl1}");
}

#[test]
fn test_tvl_single_sided() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &1000i128);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&100i128, &0i128, &p01, &p10);
    assert_eq!(tvl0, 100 * SCALE); // amount0*SCALE + 0
    assert_eq!(tvl1, 100 * SCALE); // 0 + amount0*p01
}

#[test]
fn test_tvl_swap_symmetric() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1000i128, &3i128);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&40i128, &70i128, &p01, &p10);
    // swap reserves and amounts: token0<->token1
    let (q01, q10) = lobster.get_prices_from_reserves(&3i128, &1000i128);
    let (utvl0, utvl1) = lobster.get_token_tvl_from_reserves(&70i128, &40i128, &q01, &q10);
    assert_eq!(tvl0, utvl1);
    assert_eq!(tvl1, utvl0);
}

#[test]
fn test_tvl_big_balanced() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let big = 10i128.pow(15);
    let (p01, p10) = lobster.get_prices_from_reserves(&big, &big);
    let (tvl0, tvl1) = lobster.get_token_tvl_from_reserves(&big, &big, &p01, &p10);
    assert_eq!(tvl0, 2 * big * SCALE);
    assert_eq!(tvl1, 2 * big * SCALE);
}

// get_fee_cut: invariant to the price scale, because fee_total and total_tvl
// both carry it and it cancels in current_amount * fee_total / total_tvl.

#[test]
fn test_fee_scale_invariant() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env); // fee_cut_bps = 100 (1%)

    // raw amounts stay raw in both calls; only the TVLs differ by the scale S
    let (a0, a1) = (1_000_000i128, 1_000_000i128);
    // unscaled TVLs
    let unscaled = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_100_000i128, &a0, &a1);
    // same scenario, TVLs multiplied by the price scale
    let scaled = lobster.get_fee_cut(
        &(1_000_000i128 * SCALE),
        &(1_000_000i128 * SCALE),
        &(1_100_000i128 * SCALE),
        &(1_100_000i128 * SCALE),
        &a0,
        &a1,
    );
    assert_eq!(unscaled, scaled, "fee changed with the price scale; the scale must cancel");
}

#[test]
fn test_fee_one_percent() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env); // 1%
    // profit = 200000 (total), fee_total = 2000, total_tvl = 2_200_000
    let (f0, f1) = lobster.get_fee_cut(&1_000_000i128, &1_000_000i128, &1_100_000i128, &1_100_000i128, &1_000_000i128, &1_000_000i128);
    // fee_token = 1_000_000 * 2000 / 2_200_000 = 909 (floored)
    assert_eq!(f0, 909);
    assert_eq!(f1, 909);
}

#[test]
fn test_fee_no_profit() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let flat = lobster.get_fee_cut(&1_000i128, &1_000i128, &1_000i128, &1_000i128, &500i128, &500i128);
    assert_eq!(flat, (0, 0));
}

#[test]
fn test_fee_on_loss() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let loss = lobster.get_fee_cut(&2_000i128, &2_000i128, &1_000i128, &1_000i128, &500i128, &500i128);
    assert_eq!(loss, (0, 0));
}

// end-to-end on the valuation path: an asymmetric pair must value a position at
// more than its bare token0 amount, which a truncating price would not.
#[test]
fn test_asym_worth_above_bare() {
    let env = Env::default();
    env.mock_all_auths();
    let lobster = register_lobster(&env);
    let (p01, p10) = lobster.get_prices_from_reserves(&1_000_000i128, &3_000i128);
    let (tvl0, _tvl1) = lobster.get_token_tvl_from_reserves(&500i128, &500i128, &p01, &p10);
    // holding 500 of each, token0_tvl must exceed just 500*SCALE
    assert!(tvl0 > 500 * SCALE, "the token1 leg vanished: {tvl0}");
}
