// SmartContract/lobster/src/test_soroswap.rs
#![allow(dead_code)]
#![cfg(test)]

use crate::{Lobster, LobsterClient};
use soroban_sdk::{
    Address, Env, Vec, contract, contractimpl, symbol_short, testutils::{Address as _, Ledger, LedgerInfo}, token, vec, Val, IntoVal
};

// Mock Soroswap Pool Contract (acts as LP token too)
#[contract]
pub struct MockSoroswapPool;

#[contractimpl]
impl MockSoroswapPool {
    pub fn token_0(env: Env) -> Address {
        // Deterministic address for token0
        Address::generate(&env)
    }

    // reserves default to (1000, 2000); a test can override to exercise the
    // zero-reserve guard and the reserve-dependent quote math.
    pub fn get_reserves(env: Env) -> (i128, i128) {
        let r0 = env.storage().persistent().get(&symbol_short!("R0")).unwrap_or(1000i128);
        let r1 = env.storage().persistent().get(&symbol_short!("R1")).unwrap_or(2000i128);
        (r0, r1)
    }

    pub fn set_reserves(env: Env, r0: i128, r1: i128) {
        env.storage().persistent().set(&symbol_short!("R0"), &r0);
        env.storage().persistent().set(&symbol_short!("R1"), &r1);
    }

    // LP balance of an account (pool is LP token)
    pub fn balance(_env: Env, _account: Address) -> i128 {
        1000i128
    }

    // Accept LP transfer from the contract (no-op in mock)
    pub fn transfer(_env: Env, _from: Address, _to: Address, _amount: i128) {}

    // Total LP supply
    pub fn total_supply(_env: Env) -> i128 { 10_000i128 }
}

// Mock Soroswap Router
#[contract]
pub struct MockSoroswapRouter;

#[contractimpl]
impl MockSoroswapRouter {
    pub fn initialize(env: Env, pool_address: Address) {
        env.storage().persistent().set(&symbol_short!("POOL"), &pool_address);
    }

    // Map a token pair to a pool
    pub fn router_pair_for(env: Env, _token_a: Address, _token_b: Address) -> Address {
        env.storage()
            .persistent()
            .get(&symbol_short!("POOL"))
            .unwrap_or_else(|| Address::generate(&env))
    }

    // Uniswap-style quote: amount_b = amount_a * reserve_b / reserve_a. Matches
    // the real SoroswapRouter::router_quote the contract calls.
    pub fn router_quote(_env: Env, amount_a: i128, reserve_a: i128, reserve_b: i128) -> i128 {
        amount_a * reserve_b / reserve_a
    }

    // Router add_liquidity returns (amountA, amountB, liquidity)
    pub fn add_liquidity(
        _env: Env,
        _token_a: Address,
        _token_b: Address,
        amount_a_desired: i128,
        amount_b_desired: i128,
        _amount_a_min: i128,
        _amount_b_min: i128,
        _to: Address,
        _deadline: u64,
    ) -> (i128, i128, i128) {
        // Return the desired amounts directly and a fixed liquidity
        (amount_a_desired, amount_b_desired, 1000i128)
    }

    // Router remove_liquidity returns (amountA, amountB)
    pub fn remove_liquidity(
        _env: Env,
        _token_a: Address,
        _token_b: Address,
        liquidity: i128,
        _amount_a_min: i128,
        _amount_b_min: i128,
        _to: Address,
        _deadline: u64,
    ) -> (i128, i128) {
        // Split liquidity arbitrarily into token amounts for predictability
        let a = liquidity / 2;
        let b = liquidity - a;
        (a, b)
    }
}

// Helpers
pub fn create_token_contract<'a>(e: &Env, admin: &Address) -> (token::Client<'a>, token::StellarAssetClient<'a>) {
    let sac = e.register_stellar_asset_contract_v2(admin.clone());
    (token::Client::new(e, &sac.address()), token::StellarAssetClient::new(e, &sac.address()))
}

pub fn create_mock_soroswap_pool<'a>(env: &Env) -> MockSoroswapPoolClient<'a> {
    MockSoroswapPoolClient::new(env, &env.register(MockSoroswapPool {}, ()))
}

pub fn create_mock_soroswap_router<'a>(env: &Env, pool_address: &Address) -> MockSoroswapRouterClient<'a> {
    let router_id = env.register(MockSoroswapRouter {}, ());
    let router_client = MockSoroswapRouterClient::new(env, &router_id);
    router_client.initialize(pool_address);
    router_client
}

// Test setup struct for Soroswap-specific tests
pub struct SoroswapTestSetup {
    pub env: Env,
    pub owner: Address,
    pub multisig: Address,
    pub token0_address: Address,
    pub token1_address: Address,
    pub lobster_id: Address,
    pub mock_soroswap_pool_address: Address,
    pub mock_soroswap_router_address: Address,
}

impl SoroswapTestSetup {
    pub fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 22,
            sequence_number: 10,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 1000000,
        });

        let owner = Address::generate(&env);
        let multisig = Address::generate(&env);
        let token_admin = Address::generate(&env);

        // Create tokens and mint
        let (token0, token0_admin) = create_token_contract(&env, &token_admin);
        let (token1, token1_admin) = create_token_contract(&env, &token_admin);
        token0_admin.mint(&owner, &1000000);
        token1_admin.mint(&owner, &1000000);

        // Mock pool and router first, so the vault is constructed with the mock
        // router as its trusted Soroswap router.
        let mock_pool = create_mock_soroswap_pool(&env);
        let mock_soroswap_pool_address = mock_pool.address.clone();
        let mock_router = create_mock_soroswap_router(&env, &mock_soroswap_pool_address);
        let mock_soroswap_router_address = mock_router.address.clone();

        // Deploy lobster with initialization args
        let deploy_args: Vec<Val> = vec![
            &env,
            owner.clone().into_val(&env),
            multisig.clone().into_val(&env),
            token0.address.into_val(&env),
            token1.address.into_val(&env),
            100u32.into_val(&env),
            mock_soroswap_router_address.clone().into_val(&env),
        ];

        let lobster_id = env.register(Lobster, deploy_args);

        SoroswapTestSetup {
            env,
            owner,
            multisig,
            token0_address: token0.address,
            token1_address: token1.address,
            lobster_id,
            mock_soroswap_pool_address,
            mock_soroswap_router_address,
        }
    }

    pub fn lobster_client(&self) -> LobsterClient { LobsterClient::new(&self.env, &self.lobster_id) }
    pub fn token0_client(&self) -> token::Client { token::Client::new(&self.env, &self.token0_address) }
    pub fn token1_client(&self) -> token::Client { token::Client::new(&self.env, &self.token1_address) }
    pub fn mock_soroswap_pool_client(&self) -> MockSoroswapPoolClient { MockSoroswapPoolClient::new(&self.env, &self.mock_soroswap_pool_address) }
    pub fn mock_soroswap_router_client(&self) -> MockSoroswapRouterClient { MockSoroswapRouterClient::new(&self.env, &self.mock_soroswap_router_address) }
}

// Tests
#[test]
fn test_basic_soroswap_setup() {
    let setup = SoroswapTestSetup::new();

    assert_ne!(setup.owner, setup.multisig);
    assert_ne!(setup.token0_address, setup.token1_address);
    assert_ne!(setup.lobster_id, Address::generate(&setup.env));

    let token0 = setup.token0_client();
    let token1 = setup.token1_client();
    assert_eq!(token0.balance(&setup.owner), 1000000);
    assert_eq!(token1.balance(&setup.owner), 1000000);
}

#[test]
fn test_deposit_functionality_soroswap() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    let token0 = setup.token0_client();
    let token1 = setup.token1_client();

    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    assert_eq!(token0.balance(&setup.owner), 999000);
    assert_eq!(token1.balance(&setup.owner), 998000);
    assert_eq!(token0.balance(&setup.lobster_id), 1000);
    assert_eq!(token1.balance(&setup.lobster_id), 2000);
}

#[test]
fn test_add_liquidity_soroswap() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    let token0 = setup.token0_client();
    let token1 = setup.token1_client();

    // deposit into lobster
    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    assert_eq!(token0.balance(&setup.lobster_id), 1000);
    assert_eq!(token1.balance(&setup.lobster_id), 2000);

    // call add_liquidity_soroswap
    lobster.add_liquidity_soroswap(
        &setup.owner,
        &500i128,
        &250i128,
        &0i128,
        &0i128,
        &999999999u64,
        &setup.mock_soroswap_router_address,
    );

    // mock does not actually move balances; ensure no panic
    assert_eq!(token0.balance(&setup.lobster_id), 1000);
    assert_eq!(token1.balance(&setup.lobster_id), 2000);
}

#[test]
fn test_withdraw_soroswap_basic() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();

    lobster.deposit(&setup.owner, &1000i128, &2000i128);

    // Use add_liquidity to set state before withdrawing
    lobster.add_liquidity_soroswap(
        &setup.owner,
        &500i128,
        &250i128,
        &0i128,
        &0i128,
        &999999999u64,
        &setup.mock_soroswap_router_address,
    );

    // Withdraw from soroswap router
    lobster.withdraw_soroswap(
        &setup.owner,
        &0i128,
        &0i128,
        &setup.mock_soroswap_pool_address,
        &setup.mock_soroswap_router_address,
        &999999999u64,
    );
}

#[test]
fn test_get_lp_soroswap_balance() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();

    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    let lp_balance = lobster.get_lp_soroswap(&setup.mock_soroswap_pool_address);
    assert_eq!(lp_balance, 1000);
}

// helper: register a fresh vault with explicit tokens and fee, on the setup's env.
fn register_vault(setup: &SoroswapTestSetup, token0: &Address, token1: &Address, fee_bps: u32) -> LobsterClient<'static> {
    let args: Vec<Val> = vec![
        &setup.env,
        setup.owner.clone().into_val(&setup.env),
        setup.multisig.clone().into_val(&setup.env),
        token0.into_val(&setup.env),
        token1.into_val(&setup.env),
        fee_bps.into_val(&setup.env),
        setup.mock_soroswap_router_address.clone().into_val(&setup.env),
    ];
    LobsterClient::new(&setup.env, &setup.env.register(Lobster, args))
}

// a vault is a pair of two distinct tokens; the constructor must reject a
// same-token "pair" rather than store an undefined ordering.
#[test]
#[should_panic(expected = "Error(Contract, #567)")]
fn test_constructor_rejects_identical_tokens() {
    let setup = SoroswapTestSetup::new();
    let t = setup.token0_address.clone();
    register_vault(&setup, &t, &t, 100u32);
}

// the fee is a cut of profit; the constructor caps it at 20% (2000 bps).
#[test]
#[should_panic(expected = "Error(Contract, #561)")]
fn test_constructor_rejects_fee_above_cap() {
    let setup = SoroswapTestSetup::new();
    let t0 = setup.token0_address.clone();
    let t1 = setup.token1_address.clone();
    register_vault(&setup, &t0, &t1, 2001u32);
}

// control: a fee at exactly the cap constructs cleanly.
#[test]
fn test_constructor_accepts_fee_at_cap() {
    let setup = SoroswapTestSetup::new();
    let t0 = setup.token0_address.clone();
    let t1 = setup.token1_address.clone();
    let v = register_vault(&setup, &t0, &t1, 2000u32);
    assert_eq!(v.get_owner(), setup.owner);
}

// rotating the admin is an owner power. the admin must not be able to do it.
#[test]
#[should_panic(expected = "Error(Contract, #401)")]
fn test_set_multisig_rejects_admin() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    let new_admin = Address::generate(&setup.env);
    lobster.set_multisig(&setup.multisig, &new_admin);
}

// control: the owner can rotate the admin.
#[test]
fn test_set_multisig_allows_owner() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    let new_admin = Address::generate(&setup.env);
    lobster.set_multisig(&setup.owner, &new_admin);
    assert_eq!(lobster.get_multisig(), new_admin);
}

// a withdraw must target the pool the position was opened on. a different pool,
// even with the real router, is rejected before any LP is touched.
#[test]
#[should_panic(expected = "Error(Contract, #565)")]
fn test_withdraw_soroswap_rejects_wrong_pool() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    lobster.add_liquidity_soroswap(
        &setup.owner, &500i128, &250i128, &0i128, &0i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
    let other_pool = Address::generate(&setup.env);
    lobster.withdraw_soroswap(
        &setup.owner, &0i128, &0i128,
        &other_pool, &setup.mock_soroswap_router_address, &999999999u64,
    );
}

// same for the router: the withdraw must route through the recorded router.
#[test]
#[should_panic(expected = "Error(Contract, #565)")]
fn test_withdraw_soroswap_rejects_wrong_router() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    lobster.add_liquidity_soroswap(
        &setup.owner, &500i128, &250i128, &0i128, &0i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
    let other_router = Address::generate(&setup.env);
    lobster.withdraw_soroswap(
        &setup.owner, &0i128, &0i128,
        &setup.mock_soroswap_pool_address, &other_router, &999999999u64,
    );
}



// the add path only trusts the Soroswap router set at construction. a different
// router, even a well-formed one, is rejected before any transfer is authorized.
#[test]
#[should_panic(expected = "Error(Contract, #568)")]
fn test_add_liquidity_soroswap_rejects_untrusted_router() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    let evil_pool = create_mock_soroswap_pool(&setup.env);
    let evil_router = create_mock_soroswap_router(&setup.env, &evil_pool.address);
    lobster.add_liquidity_soroswap(
        &setup.owner, &500i128, &250i128, &0i128, &0i128,
        &999999999u64, &evil_router.address,
    );
}

// get_actual_share returns the LP token address for the active position. For
// Soroswap the pair contract is the LP token, so it equals the active pool.
#[test]
fn test_get_actual_share_soroswap() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    lobster.add_liquidity_soroswap(
        &setup.owner, &500i128, &250i128, &0i128, &0i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
    assert_eq!(lobster.get_actual_share(), setup.mock_soroswap_pool_address);
}

// with no open position, get_actual_share reverts cleanly instead of decoding a
// non-Address sentinel.
#[test]
#[should_panic(expected = "Error(Contract, #566)")]
fn test_get_actual_share_no_position() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.get_actual_share();
}

// after a full open then withdraw cycle the vault is idle again and the getters
// do not trap: the Address-typed position slots were removed, not zero-poisoned.
#[test]
fn test_state_clean_after_withdraw_cycle() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    lobster.add_liquidity_soroswap(
        &setup.owner, &500i128, &250i128, &0i128, &0i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
    lobster.withdraw_soroswap(
        &setup.owner, &0i128, &0i128,
        &setup.mock_soroswap_pool_address, &setup.mock_soroswap_router_address, &999999999u64,
    );
    assert_eq!(lobster.get_active_protocol(), 5i128);
}

// the multisig admin (not only the owner) is authorized on the gated paths. this
// covers the admin branch of require_owner_or_admin, and the position opens.
#[test]
fn test_add_liquidity_soroswap_by_admin() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.add_liquidity_soroswap(
        &setup.multisig, &500i128, &250i128, &0i128, &0i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
    assert_eq!(lobster.get_active_protocol(), 0i128);
}

// get_reserves returns the pool's actual reserves (not a constant), on the only
// supported protocol (0 = Soroswap).
#[test]
fn test_get_reserves_returns_actual() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    let (r0, r1) = lobster.get_reserves(&setup.mock_soroswap_pool_address, &0u32);
    assert_eq!((r0, r1), (1000i128, 2000i128));
}

// a zero on either reserve reverts (ZeroReserves), so the price/quote math never
// divides by zero.
#[test]
#[should_panic(expected = "Error(Contract, #551)")]
fn test_get_reserves_zero_reverts() {
    let setup = SoroswapTestSetup::new();
    setup.mock_soroswap_pool_client().set_reserves(&0i128, &2000i128);
    let lobster = setup.lobster_client();
    lobster.get_reserves(&setup.mock_soroswap_pool_address, &0u32);
}

// get_reserves only supports protocol 0; any other identifier reverts.
#[test]
#[should_panic(expected = "Error(Contract, #550)")]
fn test_get_reserves_rejects_non_soroswap() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.get_reserves(&setup.mock_soroswap_pool_address, &1u32);
}

// add_liquidity_soroswap, "b optimal" branch: when the optimal amount of the
// second token falls below the caller's minimum, it reverts (InsufficientBAmount).
// with the mock reserves (1000/2000) and the pool ordering, amount1*2 = 200 is the
// optimal second leg and 200 < the 500 minimum.
#[test]
#[should_panic(expected = "Error(Contract, #406)")]
fn test_add_liquidity_soroswap_b_below_min() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.add_liquidity_soroswap(
        &setup.owner, &1000i128, &100i128, &500i128, &0i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
}

// add_liquidity_soroswap, "a optimal" branch: the other side of the selector, where
// the optimal amount of the first token falls below its minimum (InsufficientAAmount).
#[test]
#[should_panic(expected = "Error(Contract, #405)")]
fn test_add_liquidity_soroswap_a_below_min() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    lobster.add_liquidity_soroswap(
        &setup.owner, &100i128, &1000i128, &0i128, &500i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
}

// a profitable exit pays the performance fee to LOBSTER on both legs. This drives
// get_fee_cut to a positive result and exercises the fee-transfer gating in
// withdraw_soroswap: deposit, open, revalue the pool upward, then withdraw.
#[test]
fn test_withdraw_soroswap_collects_fee_on_profit() {
    let setup = SoroswapTestSetup::new();
    let lobster = setup.lobster_client();
    let token0 = setup.token0_client();
    let token1 = setup.token1_client();
    assert_eq!(token0.balance(&setup.multisig), 0);
    assert_eq!(token1.balance(&setup.multisig), 0);

    lobster.deposit(&setup.owner, &1000i128, &2000i128);
    lobster.add_liquidity_soroswap(
        &setup.owner, &500i128, &250i128, &0i128, &0i128,
        &999999999u64, &setup.mock_soroswap_router_address,
    );
    // exit values the position higher than entry, so there is a profit to fee
    setup.mock_soroswap_pool_client().set_reserves(&1000i128, &1000i128);
    lobster.withdraw_soroswap(
        &setup.owner, &0i128, &0i128,
        &setup.mock_soroswap_pool_address, &setup.mock_soroswap_router_address, &999999999u64,
    );

    assert!(token0.balance(&setup.multisig) > 0, "no fee leg reached LOBSTER on token0");
    assert!(token1.balance(&setup.multisig) > 0, "no fee leg reached LOBSTER on token1");
}
