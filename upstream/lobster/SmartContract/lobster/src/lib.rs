#![no_std]

use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contractimpl, panic_with_error, symbol_short, token, vec, Address,
    Env, I256, IntoVal, InvokeError, Symbol, Val, Vec,
};

mod error;

mod test_soroswap;
mod test_valuation;
mod test_valuation_props;

use error::ContractError;

const OWNER: Symbol = symbol_short!("OWNER");
const LOBSTER: Symbol = symbol_short!("LOBSTER");
const TOKEN0: Symbol = symbol_short!("TOKEN0");
const TOKEN1: Symbol = symbol_short!("TOKEN1");

const ACT_SHARE: Symbol = symbol_short!("ACT_SHARE");
const ACT_POOL: Symbol = symbol_short!("ACT_POOL");
const ACT_ROUTER: Symbol = symbol_short!("ACT_ROUT");
const ACT_DEX: Symbol = symbol_short!("ACT_DEX");
const KEY_RENTR: Symbol = symbol_short!("KEY_RENTR");

// trusted Soroswap router, fixed at construction. the add path only routes
// through it, so a caller cannot point the vault at a router that redirects the
// pre-authorized token transfers; the pool is derived from this trusted router.
const SW_ROUTER: Symbol = symbol_short!("SW_ROUTER");

// performance fee, and the token balances deposited so far
const FEE_CUT_BPS: Symbol = symbol_short!("FEE_BPS"); // in bps, 100 = 1%
const LAST_POOL_BALANCE_A: Symbol = symbol_short!("LAST_A");
const LAST_POOL_BALANCE_B: Symbol = symbol_short!("LAST_B");

const TOKEN0_TVL: Symbol = symbol_short!("TVL0");
const TOKEN1_TVL: Symbol = symbol_short!("TVL1");

// scale prices by 1e7 so reserve/reserve doesn't floor to 0 on a lopsided pair.
// cancels back out in get_fee_cut, so fee amounts don't change.
const PRICE_SCALE: i128 = 10_000_000;

// performance fee is a cut of profit; 20% is the ceiling we allow at construction.
// the raw contracterror only rejected above 100%, which is not a real bound for a
// fee taken from a user's gains.
const MAX_FEE_BPS: u32 = 2_000;

// TTL constants for storage extension
const DAY_IN_LEDGERS: u32 = 17280;
const MONTH_IN_LEDGERS: u32 = DAY_IN_LEDGERS * 30;
const MAX_PERSISTENT_TTL: u32 = MONTH_IN_LEDGERS * 6; // 6 months (~180 days)
const PERSISTENT_TTL_THRESHOLD: u32 = MAX_PERSISTENT_TTL - MONTH_IN_LEDGERS; // 5 months (~150 days)
const MAX_INSTANCE_TTL: u32 = MONTH_IN_LEDGERS * 6; // 6 months
const INSTANCE_TTL_THRESHOLD: u32 = MAX_INSTANCE_TTL - MONTH_IN_LEDGERS; // 5 months

// x*y/denom via a 256-bit intermediate so x*y can't overflow i128 before the div.
// inputs here are non-negative, so the div rounds toward zero.
fn mul_div(env: &Env, x: i128, y: i128, denom: i128) -> i128 {
    if denom == 0 {
        panic_with_error!(env, ContractError::ContractMathError);
    }
    I256::from_i128(env, x)
        .mul(&I256::from_i128(env, y))
        .div(&I256::from_i128(env, denom))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError))
}

#[contract]
pub struct Lobster;

#[contractimpl]
impl Lobster {
    /// Initializes the Lobster contract with owner, multisig admin, token pair, and fee configuration.
    ///
    /// # Parameters
    /// - `owner`: The address of the pool owner who can manage positions
    /// - `multisig`: The address of the multisig admin (LOBSTER) that can also manage positions
    /// - `token0`: Address of the first token in the pair
    /// - `token1`: Address of the second token in the pair
    /// - `fee_cut_bps`: performance fee in basis points, capped at MAX_FEE_BPS (2000 = 20%)
    /// - `soroswap_router`: the trusted Soroswap router the add path routes through
    ///
    /// # Panics
    /// - If contract is already initialized
    /// - If fee_cut_bps exceeds MAX_FEE_BPS
    pub fn __constructor(
        env: Env,
        owner: Address,
        multisig: Address,
        token0: Address,
        token1: Address,
        fee_cut_bps: u32,
        soroswap_router: Address,
    ) {
        if env.storage().persistent().has(&OWNER) {
            panic_with_error!(env, ContractError::AlreadyInitialized);
        }

        // a pair is two distinct tokens. the factory rejects this, but a vault
        // deployed directly must not sit in a state where correct_token_order
        // and the pool calls have no well-defined pairing.
        if token0 == token1 {
            panic_with_error!(env, ContractError::IdenticalTokens);
        }

        // fee is a cut of profit, capped at MAX_FEE_BPS
        if fee_cut_bps > MAX_FEE_BPS {
            panic_with_error!(env, ContractError::InvalidFeeValue);
        }

        env.storage().persistent().set(&OWNER, &owner);
        env.storage().persistent().set(&LOBSTER, &multisig);
        env.storage().persistent().set(&TOKEN0, &token0);
        env.storage().persistent().set(&TOKEN1, &token1);
        env.storage().persistent().set(&FEE_CUT_BPS, &fee_cut_bps);
        env.storage().persistent().set(&LAST_POOL_BALANCE_A, &0i128);
        env.storage().persistent().set(&LAST_POOL_BALANCE_B, &0i128);
        env.storage().persistent().set(&ACT_DEX, &5i128);
        env.storage().persistent().set(&SW_ROUTER, &soroswap_router);
    }

    /// Ensures that `caller` is either the pool `owner` or the multisig `admin`.
    /// Requires the caller's signature and aborts with `NotAuthorized` otherwise.
    fn require_owner_or_admin(env: &Env, caller: Address) {
        // signature first, before any storage read
        caller.require_auth();

        // Check if contract is initialized
        if !env.storage().persistent().has(&OWNER) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        let owner: Address = env
            .storage()
            .persistent()
            .get(&OWNER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        let admin: Address = env
            .storage()
            .persistent()
            .get(&LOBSTER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        if caller != owner && caller != admin {
            panic_with_error!(env.clone(), ContractError::NotAuthorized);
        }
    }

    /// Ensures that `caller` is the pool `owner` only (not the admin).
    /// Rotating the admin is an owner power: an admin able to rotate the admin
    /// could entrench itself after a key compromise.
    fn require_owner(env: &Env, caller: Address) {
        caller.require_auth();

        let owner: Address = env
            .storage()
            .persistent()
            .get(&OWNER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        if caller != owner {
            panic_with_error!(env.clone(), ContractError::NotAuthorized);
        }
    }

    /// A withdraw or close must target the pool this vault actually opened, on
    /// the expected protocol. Reading the LP token and pre-authorizing a burn or
    /// transfer against a pool the vault did not open lets that pool name an
    /// unrelated asset the vault holds; binding to ACT_POOL/ACT_DEX closes that.
    fn require_active_pool(env: &Env, pool_address: &Address, expected_dex: i128) {
        let act_dex: i128 = env.storage().persistent().get(&ACT_DEX).unwrap_or(5i128);
        if act_dex != expected_dex {
            panic_with_error!(env, ContractError::NoActivePosition);
        }
        let act_pool: Address = env
            .storage()
            .persistent()
            .get(&ACT_POOL)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NoActivePosition));
        if act_pool != *pool_address {
            panic_with_error!(env, ContractError::PoolMismatch);
        }
    }

    /// Soroswap withdraw also routes through a router; it must be the one the
    /// open position was recorded with, not an arbitrary caller-supplied router.
    fn require_active_router(env: &Env, router_address: &Address) {
        let act_router: Address = env
            .storage()
            .persistent()
            .get(&ACT_ROUTER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NoActivePosition));
        if act_router != *router_address {
            panic_with_error!(env, ContractError::PoolMismatch);
        }
    }

    /// Reentrancy guard entry: sets a temporary flag, panics if already set.
    /// This prevents reentrant calls to protected functions.
    fn enter_reentrancy(env: &Env) {
        let flag: bool = env.storage().temporary().get(&KEY_RENTR).unwrap_or(false);
        if flag {
            panic_with_error!(env, ContractError::ReentrancyDetected);
        }
        env.storage().temporary().set(&KEY_RENTR, &true);
    }

    /// Reentrancy guard exit: clears the temporary flag. Not an exported entry;
    /// reset_reentrancy is the owner/admin-gated escape hatch.
    fn exit_reentrancy(env: &Env) {
        env.storage().temporary().set(&KEY_RENTR, &false);
    }

    /// Emergency function to reset reentrancy flag if it gets stuck.
    /// This can happen if a panic occurs after `enter_reentrancy` but before `exit_reentrancy`.
    /// The flag will eventually expire with temporary storage TTL, but this provides immediate recovery.
    ///
    /// # Parameters
    /// - `caller`: The address calling this function (must be owner or admin)
    ///
    /// # Access
    /// Only owner or multisig admin
    ///
    /// # Panics
    /// - If caller is not owner or admin
    pub fn reset_reentrancy(env: Env, caller: Address) {
        Self::require_owner_or_admin(&env, caller);
        Self::exit_reentrancy(&env);
    }

    /// Helper function to extend TTL for a persistent storage key if it exists.
    /// Only extends if the key exists to avoid unnecessary operations.
    fn extend_persistent_ttl_if_exists(env: &Env, key: &Symbol) {
        if env.storage().persistent().has(key) {
            env.storage()
                .persistent()
                .extend_ttl(key, PERSISTENT_TTL_THRESHOLD, MAX_PERSISTENT_TTL);
        }
    }

    /// Extends TTL for all critical storage entries to maximum (6 months).
    /// This should be called at the start of public functions to ensure all data remains accessible.
    /// 
    /// Extends TTL for:
    /// - Contract instance (instance storage)
    /// - All persistent storage keys (OWNER, LOBSTER, TOKEN0, TOKEN1, FEE_CUT_BPS, 
    ///   ACT_DEX, ACT_SHARE, ACT_POOL, ACT_ROUTER, LAST_POOL_BALANCE_A, LAST_POOL_BALANCE_B,
    ///   TOKEN0_TVL, TOKEN1_TVL)
    ///
    /// This function is safe to call multiple times and only extends TTL if entries exist.
    fn extend_all_storage_ttl(env: &Env) {
        // Extend instance TTL (contract code and instance storage)
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, MAX_INSTANCE_TTL);

        // Extend TTL for all critical persistent storage keys (only if they exist)
        Self::extend_persistent_ttl_if_exists(env, &OWNER);
        Self::extend_persistent_ttl_if_exists(env, &LOBSTER);
        Self::extend_persistent_ttl_if_exists(env, &TOKEN0);
        Self::extend_persistent_ttl_if_exists(env, &TOKEN1);
        Self::extend_persistent_ttl_if_exists(env, &FEE_CUT_BPS);
        Self::extend_persistent_ttl_if_exists(env, &ACT_DEX);
        Self::extend_persistent_ttl_if_exists(env, &ACT_SHARE);
        Self::extend_persistent_ttl_if_exists(env, &ACT_POOL);
        Self::extend_persistent_ttl_if_exists(env, &ACT_ROUTER);
        Self::extend_persistent_ttl_if_exists(env, &LAST_POOL_BALANCE_A);
        Self::extend_persistent_ttl_if_exists(env, &LAST_POOL_BALANCE_B);
        Self::extend_persistent_ttl_if_exists(env, &TOKEN0_TVL);
        Self::extend_persistent_ttl_if_exists(env, &TOKEN1_TVL);
        Self::extend_persistent_ttl_if_exists(env, &SW_ROUTER);
    }

    /// Helper: Calculate proportional amount using ratio (numerator / denominator) * value
    /// Equivalent to: value * numerator / denominator with overflow protection
    fn calculate_proportional_amount(
        env: &Env,
        value: i128,
        numerator: i128,
        denominator: i128,
    ) -> i128 {
        mul_div(env, value, numerator, denominator)
    }

    /// Returns the address of the contract owner.
    ///
    /// # Returns
    /// The owner address stored during initialization
    ///
    /// # Panics
    /// If contract is not initialized
    pub fn get_owner(env: Env) -> Address {
        Self::extend_all_storage_ttl(&env);
        env.storage()
            .persistent()
            .get(&OWNER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    /// Returns the address of the multisig admin (LOBSTER).
    ///
    /// # Returns
    /// The multisig admin address stored during initialization
    ///
    /// # Panics
    /// If contract is not initialized
    pub fn get_multisig(env: Env) -> Address {
        Self::extend_all_storage_ttl(&env);
        env.storage()
            .persistent()
            .get(&LOBSTER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    /// Updates the multisig admin (LOBSTER) address.
    ///
    /// # Parameters
    /// - `caller`: Must be the pool owner
    /// - `new_multisig`: The new multisig admin address to store
    ///
    /// # Access
    /// Owner only
    ///
    /// # Panics
    /// - If contract is not initialized
    /// - If caller is not the owner
    pub fn set_multisig(env: Env, caller: Address, new_multisig: Address) {
        Self::extend_all_storage_ttl(&env);
        if !env.storage().persistent().has(&OWNER) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        // owner-only: the admin must not be able to rotate the admin
        Self::require_owner(&env, caller);
        env.storage()
            .persistent()
            .set(&LOBSTER, &new_multisig);
    }

    /// Returns the address of the first token (token0) in the pair.
    ///
    /// # Returns
    /// The token0 address stored during initialization
    ///
    /// # Panics
    /// If contract is not initialized
    pub fn get_token0(env: Env) -> Address {
        Self::extend_all_storage_ttl(&env);
        env.storage()
            .persistent()
            .get(&TOKEN0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    /// Returns the address of the second token (token1) in the pair.
    ///
    /// # Returns
    /// The token1 address stored during initialization
    ///
    /// # Panics
    /// If contract is not initialized
    pub fn get_token1(env: Env) -> Address {
        Self::extend_all_storage_ttl(&env);
        env.storage()
            .persistent()
            .get(&TOKEN1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    /// Returns the currently active DEX protocol identifier.
    ///
    /// # Returns
    /// Protocol identifier: 0 = Soroswap, 5 = No active position
    ///
    /// # Panics
    /// If contract is not initialized
    pub fn get_active_protocol(env: Env) -> i128 {
        Self::extend_all_storage_ttl(&env);
        env.storage()
            .persistent()
            .get(&ACT_DEX)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    /// Returns the address of the currently active liquidity pool.
    ///
    /// # Returns
    /// The pool address where liquidity is currently deployed
    ///
    /// # Panics
    /// If contract is not initialized or no active pool exists
    pub fn get_actual_pool(env: Env) -> Address {
        Self::extend_all_storage_ttl(&env);
        env.storage()
            .persistent()
            .get(&ACT_POOL)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    /// Returns the address of the currently active router (for Soroswap).
    ///
    /// # Returns
    /// The router address used for Soroswap operations
    ///
    /// # Panics
    /// If contract is not initialized or no active router exists
    pub fn get_actual_router(env: Env) -> Address {
        Self::extend_all_storage_ttl(&env);
        env.storage()
            .persistent()
            .get(&ACT_ROUTER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    /// Returns the LP/share token address for the active position, derived from
    /// the active pool and protocol. ACT_SHARE stores the share count (an i128),
    /// not an address, so it is not returned here.
    ///
    /// # Returns
    /// The LP token address representing the current liquidity position
    ///
    /// # Panics
    /// If there is no active position, or a pool query fails
    pub fn get_actual_share(env: Env) -> Address {
        Self::extend_all_storage_ttl(&env);
        // only Soroswap positions exist, where the pair contract is the LP token.
        // check the protocol before reading ACT_POOL: with no open position the
        // pool key is absent and must not be read as an Address.
        let act_dex: i128 = env.storage().persistent().get(&ACT_DEX).unwrap_or(5i128);
        if act_dex != 0 {
            panic_with_error!(env, ContractError::NoActivePosition);
        }
        env.storage()
            .persistent()
            .get(&ACT_POOL)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NoActivePosition))
    }

    /// Pull tokens from `caller` into this contract using `transfer_from`.
    /// Access: only `owner` or multisig `admin`.
    pub fn deposit(env: Env, caller: Address, amount0: i128, amount1: i128) {
        Self::extend_all_storage_ttl(&env);
        if !env.storage().persistent().has(&OWNER) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        // Validate that amounts are non-negative
        if amount0 < 0 || amount1 < 0 {
            panic_with_error!(env, ContractError::InvalidAmount);
        }

        Self::require_owner_or_admin(&env, caller.clone());
        Self::enter_reentrancy(&env);

        let token0: Address = env.storage().persistent().get(&TOKEN0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        let token1: Address = env.storage().persistent().get(&TOKEN1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        let c0 = token::Client::new(&env, &token0);
        let c1 = token::Client::new(&env, &token1);
        let to = env.current_contract_address();

        // Pull funds from user into this contract
        // transfer trom requires spender auth; contracts can authorize themselves in-call
        c0.transfer(&caller.clone(), &to.clone(), &amount0);
        c1.transfer(&caller.clone(), &to.clone(), &amount1);

        let contract_balance_a: i128 = env
            .storage()
            .persistent()
            .get(&LAST_POOL_BALANCE_A)
            .unwrap_or(0);
        let contract_balance_b: i128 = env
            .storage()
            .persistent()
            .get(&LAST_POOL_BALANCE_B)
            .unwrap_or(0);

        let new_balance_a = contract_balance_a
            .checked_add(amount0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));
        let new_balance_b = contract_balance_b
            .checked_add(amount1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        env.storage()
            .persistent()
            .set(&LAST_POOL_BALANCE_A, &new_balance_a);
        env.storage()
            .persistent()
            .set(&LAST_POOL_BALANCE_B, &new_balance_b);

        Self::exit_reentrancy(&env);

        let pos = (amount0, amount1);
        //events::emit_deposit(&env, &caller, &pos);
        env.events()
            .publish((symbol_short!("lostr_dep"), caller.clone()), &pos);
    }

    /// Public entry point for adding liquidity to a Soroswap pool via router.
    /// Validates access and delegates to internal implementation.
    ///
    /// # Parameters
    /// - `caller`: Address of the caller (must be owner or multisig admin)
    /// - `amount0`: Desired amount of token0 to add
    /// - `amount1`: Desired amount of token1 to add
    /// - `amount0_min`: Minimum amount of token0 to accept (slippage protection)
    /// - `amount1_min`: Minimum amount of token1 to accept (slippage protection)
    /// - `deadline_ledger`: Ledger number deadline for the transaction
    /// - `router_address`: Address of the Soroswap router
    ///
    /// # Access
    /// Only owner or multisig admin
    pub fn add_liquidity_soroswap(
        env: Env,
        caller: Address,
        amount0: i128,
        amount1: i128,
        amount0_min: i128,
        amount1_min: i128,
        deadline_ledger: u64,
        router_address: Address,
    ) {
        Self::extend_all_storage_ttl(&env);
        if !env.storage().persistent().has(&OWNER) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        Self::require_owner_or_admin(&env, caller.clone());

        Self::add_liquidity_soroswap_internal(
            env,
            amount0,
            amount1,
            amount0_min,
            amount1_min,
            deadline_ledger,
            router_address,
        );
    }

    /// Internal implementation for adding liquidity to a Soroswap pool via router.
    /// Handles token ordering, optimal amount calculation, and router interaction.
    ///
    /// # Parameters
    /// - `amount0`: Desired amount of token0 to add
    /// - `amount1`: Desired amount of token1 to add
    /// - `amount0_min`: Minimum amount of token0 to accept (slippage protection)
    /// - `amount1_min`: Minimum amount of token1 to accept (slippage protection)
    /// - `deadline_ledger`: Ledger number deadline for the transaction
    /// - `router_address`: Address of the Soroswap router
    ///
    /// # Panics
    /// - If active position already exists
    /// - If router/pool queries fail
    /// - If liquidity addition fails
    fn add_liquidity_soroswap_internal(
        env: Env,
        amount0: i128,
        amount1: i128,
        amount0_min: i128,
        amount1_min: i128,
        deadline_ledger: u64,
        router_address: Address,
    ) {
        Self::enter_reentrancy(&env);

        // // Check if there's already an active position
        let act_dex: i128 = env.storage().persistent().get(&ACT_DEX).unwrap_or(0i128);
        if act_dex != 5i128 {
            panic_with_error!(env, ContractError::ActivePositionExists);
        }

        let token0: Address = env.storage().persistent().get(&TOKEN0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        let token1: Address = env.storage().persistent().get(&TOKEN1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        // Approve the pool/router to spend lobster's tokens
        let _c0 = token::Client::new(&env, &token0);
        let _c1 = token::Client::new(&env, &token1);
        let spender: Address = env.current_contract_address();

        // the router must be the trusted Soroswap router set at construction.
        // the pool below is whatever this router names for the pair, so trusting
        // the router is what makes the derived pool safe as a transfer target.
        let trusted_router: Address = env
            .storage()
            .persistent()
            .get(&SW_ROUTER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        if router_address != trusted_router {
            panic_with_error!(env, ContractError::UntrustedDex);
        }

        let pool_address_r = env
            .try_invoke_contract::<Address, InvokeError>(
                &router_address,
                &Symbol::new(&env, "router_pair_for"),
                vec![&env, token0.into_val(&env), token1.into_val(&env)],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::RouterPairForFailed);
            });

        let pool_address = pool_address_r.unwrap_or_else(|_| {
            panic_with_error!(env, ContractError::RouterPairForFailed);
        });

        let pool_token0 = env
            .try_invoke_contract::<Address, InvokeError>(
                &pool_address,
                &Symbol::new(&env, "token_0"),
                vec![&env],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::PoolToken0QueryFailed);
            });

        let (
            correct_token0,
            correct_token1,
            correct_amount0,
            correct_amount1,
            correct_amount0_min,
            correct_amount1_min,
        ) = Self::correct_token_order(
            env.clone(),
            pool_token0.unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::PoolToken0QueryFailed);
            }),
            amount0,
            amount1,
            amount0_min,
            amount1_min,
        );

        let (reserve0, reserve1) = Self::get_reserves(env.clone(), pool_address.clone(), 0);

        let result1 = env
            .try_invoke_contract::<i128, InvokeError>(
                &router_address,
                &Symbol::new(&env, "router_quote"),
                vec![
                    &env,
                    correct_amount0.into_val(&env),
                    reserve0.into_val(&env),
                    reserve1.into_val(&env),
                ],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::RouterQuoteFailed);
            });

        let result0 = env
            .try_invoke_contract::<i128, InvokeError>(
                &router_address,
                &Symbol::new(&env, "router_quote"),
                vec![
                    &env,
                    correct_amount1.into_val(&env),
                    reserve1.into_val(&env),
                    reserve0.into_val(&env),
                ],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::RouterQuoteFailed);
            });

        let amount_a_optimal: i128 = result0.unwrap_or_else(|_| {
            panic_with_error!(env, ContractError::RouterQuoteFailed);
        });
        let amount_b_optimal: i128 = result1.unwrap_or_else(|_| {
            panic_with_error!(env, ContractError::RouterQuoteFailed);
        });

        // // Compute optimal amounts locally (same as router.quote)
        // let amount_b_optimal = correct_amount0
        //     .checked_mul(reserve1)
        //     .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError))
        //     .checked_div(reserve0)
        //     .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        // let amount_a_optimal = correct_amount1
        //     .checked_mul(reserve0)
        //     .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError))
        //     .checked_div(reserve1)
        //     .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        let approve0;
        let approve1;

        if amount_b_optimal <= correct_amount1 {
            if amount_b_optimal < correct_amount1_min {
                panic_with_error!(env, ContractError::InsufficientBAmount);
            }
            approve0 = correct_amount0;
            approve1 = amount_b_optimal;
        }
        // If not, we can try with the amount b desired
        else {
            // This should happen anyway. Because if we were not able to fulfill with our amount1 for our amount0
            // It is to expect that the amount_a_optimal for that lower amount1 to be lower than the amount0
            if amount_a_optimal > correct_amount0 {
                panic_with_error!(env, ContractError::ContractMathError);
            }

            if amount_a_optimal < correct_amount0_min {
                panic_with_error!(env, ContractError::InsufficientAAmount);
            }
            approve0 = amount_a_optimal;
            approve1 = correct_amount1;
        }

        let mut transfer_args_a: Vec<Val> = vec![&env];
        transfer_args_a.push_back(env.current_contract_address().into_val(&env));
        transfer_args_a.push_back(pool_address.into_val(&env));
        transfer_args_a.push_back(approve0.into_val(&env));

        let mut transfer_args_b: Vec<Val> = vec![&env];
        transfer_args_b.push_back(env.current_contract_address().into_val(&env));
        transfer_args_b.push_back(pool_address.into_val(&env));
        transfer_args_b.push_back(approve1.into_val(&env));

        env.authorize_as_current_contract(vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: correct_token0.clone(),
                    fn_name: Symbol::new(&env, "transfer"),
                    args: transfer_args_a,
                },
                sub_invocations: vec![&env],
            }),
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: correct_token1.clone(),
                    fn_name: Symbol::new(&env, "transfer"),
                    args: transfer_args_b,
                },
                sub_invocations: vec![&env],
            }),
        ]);

        let liquidity = env
            .try_invoke_contract::<(i128, i128, i128), InvokeError>(
                &router_address,
                &Symbol::new(&env, "add_liquidity"),
                vec![
                    &env,
                    correct_token0.into_val(&env),
                    correct_token1.into_val(&env),
                    correct_amount0.into_val(&env),
                    correct_amount1.into_val(&env),
                    correct_amount0_min.into_val(&env),
                    correct_amount1_min.into_val(&env),
                    spender.into_val(&env),
                    deadline_ledger.into_val(&env),
                ],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::RouterAddLiquidityFailed);
            });

        let (price0_in_1, price1_in_0) =
            Self::get_prices_from_reserves(env.clone(), reserve0, reserve1);

        // baseline from approve0/approve1 (what the router pulls), not the raw
        // requested correct_amount0/1: the router accepts an inflated desired and
        // pulls only the optimal, so a requested baseline could skip the fee.
        let (token0_tvl, token1_tvl) = Self::get_token_tvl_from_reserves(
            env.clone(),
            approve0.clone(),
            approve1.clone(),
            price0_in_1.clone(),
            price1_in_0.clone(),
        );

        env.storage().persistent().set(&TOKEN0_TVL, &token0_tvl);
        env.storage().persistent().set(&TOKEN1_TVL, &token1_tvl);

        let (_token0, _token1, liquidity_shares) = liquidity.unwrap_or_else(|_| {
            panic_with_error!(env, ContractError::RouterAddLiquidityFailed);
        });

        Self::save_n_publish(
            env.clone(),
            pool_address.clone(),
            liquidity_shares,
            approve0,
            approve1,
            "add_liq_soro",
            0i128,
        );

        env.storage()
            .persistent()
            .set(&ACT_ROUTER, &router_address.clone());

        Self::exit_reentrancy(&env);
    }

    /// Public entry point for withdrawing all liquidity from a Soroswap pool via router.
    /// Validates access and delegates to internal implementation.
    ///
    /// # Parameters
    /// - `caller`: Address of the caller (must be owner or multisig admin)
    /// - `amount0_min`: Minimum amount of token0 to receive (slippage protection)
    /// - `amount1_min`: Minimum amount of token1 to receive (slippage protection)
    /// - `pool_address`: Address of the Soroswap pool to withdraw from
    /// - `router_address`: Address of the Soroswap router
    /// - `deadline`: Ledger number deadline for the transaction
    ///
    /// # Access
    /// Only owner or multisig admin
    pub fn withdraw_soroswap(
        env: Env,
        caller: Address,
        amount0_min: i128,
        amount1_min: i128,
        pool_address: Address,
        router_address: Address,
        deadline: u64,
    ) {
        Self::extend_all_storage_ttl(&env);
        if !env.storage().persistent().has(&OWNER) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        Self::require_owner_or_admin(&env, caller.clone());

        Self::withdraw_soroswap_internal(
            env,
            amount0_min,
            amount1_min,
            pool_address,
            router_address,
            deadline,
        );
    }
    /// Internal implementation for withdrawing all liquidity from a Soroswap pool via router.
    /// Transfers LP tokens to pool and removes liquidity, calculates and transfers fees.
    ///
    /// # Parameters
    /// - `amount0_min`: Minimum amount of token0 to receive (slippage protection)
    /// - `amount1_min`: Minimum amount of token1 to receive (slippage protection)
    /// - `pool_address`: Address of the Soroswap pool to withdraw from
    /// - `router_address`: Address of the Soroswap router
    /// - `deadline`: Ledger number deadline for the transaction
    ///
    /// # Panics
    /// - If pool queries fail
    /// - If liquidity removal fails
    /// - If amounts received are below minimums
    fn withdraw_soroswap_internal(
        env: Env,
        amount0_min: i128,
        amount1_min: i128,
        pool_address: Address,
        router_address: Address,
        deadline: u64,
    ) {
        Self::enter_reentrancy(&env);

        // only the pool and router this vault opened on Soroswap, while a
        // position is open
        Self::require_active_pool(&env, &pool_address, 0i128);
        Self::require_active_router(&env, &router_address);

        let spender: Address = env.current_contract_address();

        let pool_token0 = env
            .try_invoke_contract::<Address, InvokeError>(
                &pool_address,
                &Symbol::new(&env, "token_0"),
                vec![&env],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::PoolToken0QueryFailed);
            });

        let (
            correct_token0,
            correct_token1,
            _correct_amount0,
            _correct_amount1,
            correct_amount0_min,
            correct_amount1_min,
        ) = Self::correct_token_order(
            env.clone(),
            pool_token0.unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::PoolToken0QueryFailed);
            }),
            0i128, // amounts unused here, only the mins
            0i128,
            amount0_min,
            amount1_min,
        );

        let (reserve0, reserve1) = Self::get_reserves(env.clone(), pool_address.clone(), 0);

        let liquidity: i128 = Self::get_lp_soroswap(env.clone(), pool_address.clone());

        let mut transfer_args: Vec<Val> = vec![&env];
        transfer_args.push_back(env.current_contract_address().into_val(&env));
        transfer_args.push_back(pool_address.into_val(&env));
        transfer_args.push_back(liquidity.into_val(&env));

        env.authorize_as_current_contract(vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: pool_address.clone(),
                    fn_name: Symbol::new(&env, "transfer"),
                    args: transfer_args,
                },
                sub_invocations: vec![&env],
            }),
        ]);

        let mut withdraw_args: Vec<Val> = vec![&env];
        withdraw_args.push_back(correct_token0.into_val(&env));
        withdraw_args.push_back(correct_token1.into_val(&env));
        withdraw_args.push_back(liquidity.into_val(&env));
        withdraw_args.push_back(correct_amount0_min.into_val(&env));
        withdraw_args.push_back(correct_amount1_min.into_val(&env));
        withdraw_args.push_back(spender.into_val(&env));
        withdraw_args.push_back(deadline.into_val(&env));

        let withdraw_result = env
            .try_invoke_contract::<(i128, i128), InvokeError>(
                &router_address,
                &Symbol::new(&env, "remove_liquidity"),
                withdraw_args,
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::RouterRemoveLiquidityFailed);
            })
            .expect("Conversion error");

        let (correct_amount0, correct_amount1) = withdraw_result;

        let (price0_in_1, price1_in_0) =
            Self::get_prices_from_reserves(env.clone(), reserve0, reserve1);

        let (token0_tvl, token1_tvl) = Self::get_token_tvl_from_reserves(
            env.clone(),
            correct_amount0.clone(),
            correct_amount1.clone(),
            price0_in_1.clone(),
            price1_in_0.clone(),
        );

        let init_token0_tvl: i128 = env.storage().persistent().get(&TOKEN0_TVL).unwrap();
        let init_token1_tvl: i128 = env.storage().persistent().get(&TOKEN1_TVL).unwrap();

        // a negative init TVL would bill a fee bigger than the position
        if init_token0_tvl.clone() < 0 || init_token1_tvl.clone() < 0 {
            panic_with_error!(env, ContractError::ContractMathError);
        }

        let (amount0_fee_cut, amount1_fee_cut) = Self::get_fee_cut(
            env.clone(),
            init_token0_tvl.clone(),
            init_token1_tvl.clone(),
            token0_tvl.clone(),
            token1_tvl.clone(),
            correct_amount0.clone(),
            correct_amount1.clone(),
        );

        if amount0_fee_cut > 0 {
            Self::transfer_fee_cut(env.clone(), correct_token0, amount0_fee_cut);
        }
        if amount1_fee_cut > 0 {
            Self::transfer_fee_cut(env.clone(), correct_token1, amount1_fee_cut);
        }

        env.storage().persistent().set(&TOKEN0_TVL, &0i128);
        env.storage().persistent().set(&TOKEN1_TVL, &0i128);
        // remove rather than zero: these keys are Address-typed, and writing an
        // integer sentinel would make the get_actual_* getters trap on a type
        // mismatch after a cycle. absent reads back cleanly as no position.
        env.storage().persistent().remove(&ACT_POOL);
        env.storage().persistent().remove(&ACT_SHARE);
        env.storage().persistent().remove(&ACT_ROUTER);
        env.storage().persistent().set(&ACT_DEX, &5i128);

        Self::exit_reentrancy(&env);
    }

    /// Returns the current LP token balance for a Soroswap pool.
    /// In Soroswap, the LP token is the pool address itself.
    ///
    /// # Parameters
    /// - `pool_address`: Address of the Soroswap pool (also the LP token address)
    ///
    /// # Returns
    /// Current LP token balance held by this contract
    ///
    /// # Panics
    /// If balance query fails
    pub fn get_lp_soroswap(env: Env, pool_address: Address) -> i128 {
        Self::extend_all_storage_ttl(&env);
        let lp_balance = env
            .try_invoke_contract::<i128, InvokeError>(
                &pool_address,
                &Symbol::new(&env, "balance"),
                vec![&env, env.current_contract_address().into_val(&env)],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::PoolLpBalanceQueryFailed);
            });

        let liquidity = lp_balance.unwrap();

        liquidity
    }

    /// Public entry point for withdrawing tokens directly from the contract balance.
    /// Transfers tokens from contract to owner without interacting with pools.
    ///
    /// # Parameters
    /// - `caller`: Address of the caller (must be owner or multisig admin)
    /// - `amount0`: Amount of token0 to withdraw
    /// - `amount1`: Amount of token1 to withdraw
    ///
    /// # Access
    /// Only owner or multisig admin
    pub fn withdraw_contract(env: Env, caller: Address, amount0: i128, amount1: i128) {
        Self::extend_all_storage_ttl(&env);
        if !env.storage().persistent().has(&OWNER) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        Self::require_owner_or_admin(&env, caller.clone());

        Self::withdraw_contract_internal(
            env.clone(),
            amount0.clone(),
            amount1.clone(),
        );
    }

    /// Internal implementation for withdrawing tokens directly from contract balance.
    /// Transfers tokens from contract to owner without interacting with pools.
    ///
    /// # Parameters
    /// - `caller`: Address of the caller (must be owner or multisig admin)
    /// - `amount0`: Amount of token0 to withdraw
    /// - `amount1`: Amount of token1 to withdraw
    ///
    /// # Panics
    /// - If amounts are negative
    /// - If token transfers fail
    fn withdraw_contract_internal(env: Env, amount0: i128, amount1: i128) {
        // guard for parity with the other fund-moving paths, even though the
        // destination is always the stored owner and the tokens are the trusted pair
        Self::enter_reentrancy(&env);

        // Validate that amounts are non-negative
        if amount0 < 0 || amount1 < 0 {
            panic_with_error!(env, ContractError::InvalidAmount);
        }

        let token0: Address = env.storage().persistent().get(&TOKEN0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        let token1: Address = env.storage().persistent().get(&TOKEN1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        let spender: Address = env.current_contract_address();

        let owner: Address = env.storage().persistent().get(&OWNER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        let mut withdraw_args0: Vec<Val> = vec![&env];
        withdraw_args0.push_back(spender.into_val(&env));
        withdraw_args0.push_back(owner.into_val(&env));
        withdraw_args0.push_back(amount0.into_val(&env));

        let mut withdraw_args1: Vec<Val> = vec![&env];
        withdraw_args1.push_back(spender.into_val(&env));
        withdraw_args1.push_back(owner.into_val(&env));
        withdraw_args1.push_back(amount1.into_val(&env));

        let _withdraw = env
            .try_invoke_contract::<(i128, i128), InvokeError>(
                &token0,
                &Symbol::new(&env, "transfer"),
                withdraw_args0,
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::TokenTransferFailed);
            });

        let _withdraw = env
            .try_invoke_contract::<(i128, i128), InvokeError>(
                &token1,
                &Symbol::new(&env, "transfer"),
                withdraw_args1,
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::TokenTransferFailed);
            });

        Self::exit_reentrancy(&env);
    }

    /// Corrects token and amount ordering to match the pool's expected token order.
    /// Ensures amounts are aligned with pool's token0/token1 ordering.
    ///
    /// # Parameters
    /// - `pool_token0`: Address of token0 as defined by the pool
    /// - `amount0`: Amount of token0 (as defined by contract)
    /// - `amount1`: Amount of token1 (as defined by contract)
    /// - `amount0_min`: Minimum amount of token0
    /// - `amount1_min`: Minimum amount of token1
    ///
    /// # Returns
    /// Tuple of (correct_token0, correct_token1, correct_amount0, correct_amount1, correct_amount0_min, correct_amount1_min)
    /// ordered to match the pool's token ordering
    fn correct_token_order(
        env: Env,
        pool_token0: Address,
        amount0: i128,
        amount1: i128,
        amount0_min: i128,
        amount1_min: i128,
    ) -> (Address, Address, i128, i128, i128, i128) {
        let token0: Address = env.storage().persistent().get(&TOKEN0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        let token1: Address = env.storage().persistent().get(&TOKEN1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        if pool_token0 != token0 {
            (
                token1,      // correct_token0
                token0,      // correct_token1
                amount1,     // correct_amount0
                amount0,     // correct_amount1
                amount1_min, // correct_amount0_min
                amount0_min, // correct_amount1_min
            )
        } else {
            (
                token0,      // correct_token0
                token1,      // correct_token1
                amount0,     // correct_amount0
                amount1,     // correct_amount1
                amount0_min, // correct_amount0_min
                amount1_min, // correct_amount1_min
            )
        }
    }

    /// Saves liquidity position state and publishes an event.
    /// Updates storage with pool address, LP shares, amounts, and protocol identifier.
    ///
    /// # Parameters
    /// - `pool_address`: Address of the liquidity pool
    /// - `liquidity_shares`: Amount of LP tokens received/held
    /// - `correct_amount0`: Amount of token0 deposited/withdrawn
    /// - `correct_amount1`: Amount of token1 deposited/withdrawn
    /// - `event_name`: Name of the event to publish
    /// - `protocol`: Protocol identifier (0 = Soroswap)
    fn save_n_publish(
        env: Env,
        pool_address: Address,
        liquidity_shares: i128,
        correct_amount0: i128,
        correct_amount1: i128,
        event_name: &str,
        protocol: i128,
    ) {
        env.events().publish(
            (Symbol::new(&env, event_name), pool_address.clone()),
            (liquidity_shares, correct_amount0, correct_amount1),
        );

        env.storage()
            .persistent()
            .set(&ACT_SHARE, &liquidity_shares);

        env.storage().persistent().set(&ACT_POOL, &pool_address);

        env.storage()
            .persistent()
            .set(&LAST_POOL_BALANCE_A, &correct_amount0);

        env.storage()
            .persistent()
            .set(&LAST_POOL_BALANCE_B, &correct_amount1);

        env.storage().persistent().set(&ACT_DEX, &protocol);
    }

    /// Retrieves token reserves from a pool, handling different DEX protocols.
    /// Returns reserves in pool order (not contract order) to maintain consistency
    /// with amounts from correct_token_order and TVL calculations.
    ///
    /// # Parameters
    /// - `pool_address`: Address of the liquidity pool
    /// - `protocol_tvl`: Protocol identifier (0 = Soroswap, the only supported protocol)
    ///
    /// # Returns
    /// Tuple of (reserve0, reserve1) in pool order (pool's token0, pool's token1)
    ///
    /// # Panics
    /// - If protocol is invalid
    /// - If reserves are zero
    /// - If pool query fails
    pub fn get_reserves(env: Env, pool_address: Address, protocol_tvl: u32) -> (i128, i128) {
        Self::extend_all_storage_ttl(&env);
        // only Soroswap (protocol 0) is executed by this contract
        if protocol_tvl != 0 {
            panic_with_error!(env, ContractError::InvalidProtocol);
        }
        // Soroswap returns reserves directly, already in pool order
        let (reserve0, reserve1) = env
            .try_invoke_contract::<(i128, i128), InvokeError>(
                &pool_address,
                &Symbol::new(&env, "get_reserves"),
                vec![&env],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::QueryPoolInfoFailed);
            })
            .expect("Conversion error");

        // Check for division by zero
        if reserve0 == 0 || reserve1 == 0 {
            panic_with_error!(env, ContractError::ZeroReserves);
        }

        (reserve0, reserve1)
    }

    /// Calculates token prices from pool reserves.
    ///
    /// # Parameters
    /// - `reserve0`: Reserve amount of token0 in the pool
    /// - `reserve1`: Reserve amount of token1 in the pool
    ///
    /// # Returns
    /// Tuple of (price_0_in_1, price_1_in_0) where:
    /// - price_0_in_1: Price of token0 denominated in token1
    /// - price_1_in_0: Price of token1 denominated in token0
    ///
    /// # Panics
    /// - If reserves are zero (division by zero)
    /// - On arithmetic overflow
    pub fn get_prices_from_reserves(env: Env, reserve0: i128, reserve1: i128) -> (i128, i128) {
        Self::extend_all_storage_ttl(&env);
        if reserve0 == 0 || reserve1 == 0 {
            panic_with_error!(env, ContractError::ZeroReserves);
        }
        // scaled by PRICE_SCALE so the fractional side of an asymmetric pair does
        // not divide down to zero
        let price_0_in_1 = mul_div(&env, reserve1, PRICE_SCALE, reserve0);
        let price_1_in_0 = mul_div(&env, reserve0, PRICE_SCALE, reserve1);

        (price_0_in_1, price_1_in_0)
    }

    /// Calculates Total Value Locked (TVL) for both tokens in the position.
    /// TVL represents the total value of tokens, accounting for their relative prices.
    ///
    /// # Parameters
    /// - `amount0`: Amount of token0
    /// - `amount1`: Amount of token1
    /// - `price0_in_1`: Price of token0 in terms of token1
    /// - `price1_in_0`: Price of token1 in terms of token0
    ///
    /// # Returns
    /// Tuple of (token0_tvl, token1_tvl) where:
    /// - token0_tvl: Total value of position in token0 terms
    /// - token1_tvl: Total value of position in token1 terms
    ///
    /// # Panics
    /// On arithmetic overflow
    pub fn get_token_tvl_from_reserves(
        env: Env,
        amount0: i128,
        amount1: i128,
        price0_in_1: i128,
        price1_in_0: i128,
    ) -> (i128, i128) {
        Self::extend_all_storage_ttl(&env);
        // prices come in scaled by PRICE_SCALE, so scale the raw amount too before
        // adding the converted leg. both legs have to count.
        let token0_tvl = mul_div(&env, amount0, PRICE_SCALE, 1)
            .checked_add(mul_div(&env, amount1, price1_in_0, 1))
            .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        let token1_tvl = mul_div(&env, amount1, PRICE_SCALE, 1)
            .checked_add(mul_div(&env, amount0, price0_in_1, 1))
            .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        (token0_tvl, token1_tvl)
    }

    /// Fee taken on the position's gain, measured in a single numeraire (token0).
    /// Profit is current_tvl0 - init_tvl0 (both value the whole position in token0
    /// terms); the fee is that gain times the bps, taken in kind as an equal
    /// fraction of each held token.
    ///
    /// # Parameters
    /// - `init_tvl0`: token0-denominated value of the position when it was opened
    /// - `init_tvl1`: token1-denominated value at open (only the sign is checked)
    /// - `current_tvl0`: token0-denominated value now
    /// - `current_tvl1`: token1-denominated value now (unused in the profit metric)
    /// - `current_amount0`: Current amount of token0
    /// - `current_amount1`: Current amount of token1
    ///
    /// # Returns
    /// Tuple of (fee_token0, fee_token1) - fee amounts in each token
    /// Returns (0, 0) if the position did not gain value in token0 terms
    ///
    /// # Panics
    /// On arithmetic overflow, or if an initial TVL is negative
    pub fn get_fee_cut(
        env: Env,
        init_tvl0: i128,
        init_tvl1: i128,
        current_tvl0: i128,
        current_tvl1: i128,
        current_amount0: i128,
        current_amount1: i128,
    ) -> (i128, i128) {
        Self::extend_all_storage_ttl(&env);
        let fee_cut_bps: u32 = env.storage().persistent().get(&FEE_CUT_BPS).unwrap();

        // init tvl is written non-negative at deposit. a negative value here makes
        // total_profit exceed total_tvl and bills a fee bigger than the position.
        // the withdraw call sites reject it already; the pub calculator must too.
        if init_tvl0 < 0 || init_tvl1 < 0 {
            panic_with_error!(env, ContractError::ContractMathError);
        }

        // profit is measured in one numeraire, token0. token0_tvl already values
        // the whole position (both legs) in token0 terms, so current_tvl0 -
        // init_tvl0 is the real change in value. summing a token0-unit leg with a
        // token1-unit leg would let a pure price move (one leg up, the other down)
        // invent profit on a loss and hide it on a gain. current_tvl1 is kept in
        // the signature for the withdraw call sites but not summed.
        let _ = current_tvl1;
        let profit = current_tvl0
            .checked_sub(init_tvl0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        if profit <= 0 {
            return (0, 0); // no fee unless the position gained value in token0 terms
        }

        // mul_div so profit * bps can't overflow i128 before the /10000
        let fee_value = mul_div(&env, profit, fee_cut_bps as i128, 10_000);

        if current_tvl0 == 0 {
            return (0, 0);
        }

        // take fee_value worth of the position in kind: the same fraction
        // fee_value / current_tvl0 of each held token. both are token0-scaled, so
        // the fraction is dimensionless and the price scale cancels.
        let fee_token0 = mul_div(&env, current_amount0, fee_value, current_tvl0);
        let fee_token1 = mul_div(&env, current_amount1, fee_value, current_tvl0);

        (fee_token0, fee_token1)
    }

    /// Transfers the calculated fee amount to the multisig admin (LOBSTER) address.
    /// Uses contract authorization to enable the token transfer.
    ///
    /// # Parameters
    /// - `correct_token`: Address of the token to transfer
    /// - `amount_fee_cut`: Amount of tokens to transfer as fee
    ///
    /// # Panics
    /// If token transfer fails
    fn transfer_fee_cut(env: Env, correct_token: Address, amount_fee_cut: i128) {
        let lobster_address: Address = env.storage().persistent().get(&LOBSTER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        // the vault authorizes the token's transfer out of its own balance
        env.authorize_as_current_contract(vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: correct_token.clone(),
                    fn_name: Symbol::new(&env, "transfer"),
                    args: vec![
                        &env,
                        env.current_contract_address().into_val(&env),
                        lobster_address.into_val(&env),
                        amount_fee_cut.into_val(&env),
                    ],
                },
                sub_invocations: vec![&env],
            }),
        ]);

        let mut transfer_args: Vec<Val> = vec![&env];
        transfer_args.push_back(env.current_contract_address().into_val(&env));
        transfer_args.push_back(lobster_address.into_val(&env));
        transfer_args.push_back(amount_fee_cut.into_val(&env));

        let _transfer = env
            .try_invoke_contract::<(), InvokeError>(
                &correct_token,
                &Symbol::new(&env, "transfer"),
                transfer_args,
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::TokenTransferFailed);
            });
    }

    /// Retrieves the underlying token amounts represented by LP position in a Soroswap pool.
    /// Calculates amounts proportionally based on LP share of total supply.
    ///
    /// # Parameters
    /// - `pool_address`: Address of the Soroswap pool
    ///
    /// # Returns
    /// Tuple of (amount_a, amount_b) ordered to match contract's token0/token1
    ///
    /// # Panics
    /// If pool query fails or calculation overflows
    pub fn get_amounts_from_soroswap(env: Env, pool_address: Address) -> (i128, i128) {
        Self::extend_all_storage_ttl(&env);
        let lp_balance = Self::get_lp_soroswap(env.clone(), pool_address.clone());

        let (reserve0, reserve1) = Self::get_reserves(env.clone(), pool_address.clone(), 0);

        let total_lp = env
            .try_invoke_contract::<i128, InvokeError>(
                &pool_address.clone(),
                &Symbol::new(&env, "total_supply"),
                vec![&env],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::LpTokenBalanceQueryFailed);
            });

        let total_lp = total_lp.unwrap();

        let amount_a = if total_lp != 0 {
            Self::calculate_proportional_amount(&env, reserve0, lp_balance, total_lp)
        } else {
            0i128
        };
        let amount_b = if total_lp != 0 {
            Self::calculate_proportional_amount(&env, reserve1, lp_balance, total_lp)
        } else {
            0i128
        };

        (amount_a, amount_b)
    }

    /// Returns the current token balances held directly by this contract (not in pools).
    ///
    /// # Returns
    /// Tuple of (amount0, amount1) - current balances of token0 and token1 in contract
    ///
    /// # Panics
    /// If token balance queries fail
    pub fn get_amounts_tokens(env: Env) -> (i128, i128) {
        Self::extend_all_storage_ttl(&env);
        let token0: Address = env.storage().persistent().get(&TOKEN0)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        let token1: Address = env.storage().persistent().get(&TOKEN1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        let amount0 = env
            .try_invoke_contract::<i128, InvokeError>(
                &token0.clone(),
                &Symbol::new(&env, "balance"),
                vec![&env, env.current_contract_address().into_val(&env)],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::LpTokenBalanceQueryFailed);
            });

        let amount1 = env
            .try_invoke_contract::<i128, InvokeError>(
                &token1.clone(),
                &Symbol::new(&env, "balance"),
                vec![&env, env.current_contract_address().into_val(&env)],
            )
            .unwrap_or_else(|_| {
                panic_with_error!(env, ContractError::LpTokenBalanceQueryFailed);
            });

        (amount0.unwrap(), amount1.unwrap())
    }
}
