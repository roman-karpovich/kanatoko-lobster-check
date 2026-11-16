#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, vec, xdr::ToXdr, Address,
    Bytes, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

mod error;
mod test;

use error::ContractError;

#[contracttype]
#[derive(Clone)]
pub struct LobsterPools {
    /// The asset A in the pool together with asset amounts
    token0: Address,
    /// The asset B in the pool together with asset amounts
    token1: Address,
    /// The address of the lobster contract for the liquidity pool
    lobster_address: Address,
    /// The address of the owner of the pool
    owner: Address,
}

const KEY_RENTR: Symbol = symbol_short!("KEY_RENTR");

const ADMIN: Symbol = symbol_short!("admin");
const LOBSTER_POOLS_ID: Symbol = symbol_short!("lob_pl_id");
const LOBSTER_POOLS_ADR: Symbol = symbol_short!("lob_pl_ad");
const LOBSTER_POOL_COUNT: Symbol = symbol_short!("lob_count");
const LOBSTER_POOL_USER: Symbol = symbol_short!("lob_adr");
const LOBSTER_POOL_TOKEN: Symbol = symbol_short!("lob_token");

const LOBSTER_WASM_HASH: Symbol = symbol_short!("lob_hash");

// the current fee, and a change to it waiting out its notice
const PENDING_FEE_CHANGE: Symbol = symbol_short!("PEND_FEE");
const FEES: Symbol = symbol_short!("FEES");

// TTL: keep the factory's own state alive. without this the singleton keys
// (admin, wasm hash, pool count, fee) expire under rent and create_pool starts
// reverting. mirrors the vault's 6-month horizon.
const DAY_IN_LEDGERS: u32 = 17280;
const MONTH_IN_LEDGERS: u32 = DAY_IN_LEDGERS * 30;
const MAX_TTL: u32 = MONTH_IN_LEDGERS * 6;
const TTL_THRESHOLD: u32 = MAX_TTL - MONTH_IN_LEDGERS;

// ceiling on the performance fee the factory sets or announces, 20%. matches the
// vault's own MAX_FEE_BPS.
const MAX_FEE_BPS: u32 = 2_000;

// trusted Soroswap router for the network, set once at construction and baked
// into every vault the factory deploys, so a vault never trusts a caller-supplied
// router for the add path.
const SW_ROUTER: Symbol = symbol_short!("SW_ROUTER");

// kept under PENDING_FEE_CHANGE until execute_fee_change applies it
#[contracttype]
#[derive(Clone)]
pub struct PendingFeeChange {
    /// The new fee value in basis points
    new_fee_bps: u32,
    /// The timestamp when the change was announced
    announcement_timestamp: u64,
    /// The timestamp when the change can be executed (announcement + delay)
    execution_timestamp: u64,
}

#[contract]
pub struct Factory;

#[contractimpl]
impl Factory {
    /// Reentrancy guard entry: sets a temporary flag, returns false if already set.
    fn enter_reentrancy(env: &Env) -> bool {
        let flag: bool = env.storage().temporary().get(&KEY_RENTR).unwrap_or(false);
        if flag {
            return false;
        }
        env.storage().temporary().set(&KEY_RENTR, &true);
        true
    }

    /// Reentrancy guard exit: clears the temporary flag. Not an exported entry.
    fn exit_reentrancy(env: &Env) {
        env.storage().temporary().set(&KEY_RENTR, &false);
    }

    /// Extend the TTL of the factory instance and its singleton state keys.
    /// Called from the state-changing entries so the factory does not expire
    /// while it is in use. Per-pool index entries are not bumped here (unbounded);
    /// the off-chain monitor and per-pool reads keep those alive.
    fn extend_state_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, MAX_TTL);
        for key in [&ADMIN, &LOBSTER_WASM_HASH, &LOBSTER_POOL_COUNT, &FEES, &SW_ROUTER] {
            if env.storage().persistent().has(key) {
                env.storage()
                    .persistent()
                    .extend_ttl(key, TTL_THRESHOLD, MAX_TTL);
            }
        }
    }

    /// Emergency reset of the reentrancy flag if a panic left it stuck. Admin only,
    /// so a stranger cannot clear the guard mid-call.
    pub fn reset_reentrancy(env: Env) {
        let admin: Address = env
            .storage()
            .persistent()
            .get::<Symbol, Address>(&ADMIN)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        admin.require_auth();
        Self::exit_reentrancy(&env);
    }

    /// Initializes the Factory contract with admin, WASM hash for deployment, and fee configuration.
    /// 
    /// # Parameters
    /// - `admin`: The address of the factory admin who can manage settings
    /// - `wasm_hash`: Hash of the Lobster contract WASM for deterministic deployment
    /// - `fee_bps`: default performance fee in basis points, capped at MAX_FEE_BPS (2000 = 20%)
    /// - `soroswap_router`: trusted Soroswap router baked into every deployed vault
    /// 
    /// # Panics
    /// - If contract is already initialized
    /// - If fee_bps exceeds MAX_FEE_BPS
    pub fn __constructor(
        env: Env,
        admin: Address,
        wasm_hash: BytesN<32>,
        fee_bps: u32,
        soroswap_router: Address,
    ) {
        // Prevent re-initialization: check if contract is already initialized
        if env.storage().persistent().has(&ADMIN) {
            panic_with_error!(env, ContractError::AlreadyInitialized);
        }

        admin.require_auth();

        // fee is a cut of profit, capped at MAX_FEE_BPS
        if fee_bps > MAX_FEE_BPS {
            panic_with_error!(env, ContractError::InvalidFeeValue);
        }

        env.storage().persistent().set(&ADMIN, &admin);
        env.storage().persistent().set(&SW_ROUTER, &soroswap_router);

        let lobster_pools: Vec<LobsterPools> = Vec::new(&env);

        env.storage()
            .persistent()
            .set(&LOBSTER_POOLS_ID, &lobster_pools);
        env.storage()
            .persistent()
            .set(&LOBSTER_POOLS_ADR, &lobster_pools);

        env.storage().persistent().set(&LOBSTER_POOL_COUNT, &0u64);

        env.storage()
            .persistent()
            .set(&LOBSTER_WASM_HASH, &wasm_hash);

        env.storage().persistent().set(&FEES, &fee_bps);
    }

    /// Creates a new Lobster pool for a user with the specified token pair.
    /// Deploys a new Lobster contract instance deterministically and registers it.
    /// 
    /// # Parameters
    /// - `user`: Address of the user who will own the pool
    /// - `token0`: Address of the first token in the pair
    /// - `token1`: Address of the second token in the pair
    /// 
    /// # Returns
    /// The address of the newly deployed Lobster contract
    /// 
    /// # Panics
    /// - If contract is not initialized
    /// - If token0 and token1 are the same
    /// - If a pool already exists for this user and token pair
    /// - If reentrancy is detected
    pub fn create_pool(env: Env, user: Address, token0: Address, token1: Address) -> Address {
        if !env.storage().persistent().has(&ADMIN) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        Self::extend_state_ttl(&env);

        if !Self::enter_reentrancy(&env) {
            panic_with_error!(env, ContractError::AlreadyExists); // re-entered mid create_pool
        }
        user.require_auth();

        // Validate that token0 and token1 are distinct
        if token0 == token1 {
            panic_with_error!(env, ContractError::InvalidPoolTokens);
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get::<Symbol, Address>(&ADMIN)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        let lobster_pool_count: u64 = env
            .storage()
            .persistent()
            .get::<Symbol, u64>(&LOBSTER_POOL_COUNT)
            .unwrap_or(0);

        let mut lobster_user = env
            .storage()
            .persistent()
            .get::<(Symbol, Address), Vec<LobsterPools>>(&(LOBSTER_POOL_USER, user.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        //get lobster token from storage
        let mut lobster_token = env
            .storage()
            .persistent()
            .get::<(Symbol, Address, Address), Vec<LobsterPools>>(&(
                LOBSTER_POOL_TOKEN,
                token0.clone(),
                token1.clone(),
            ))
            .unwrap_or_else(|| Vec::new(&env));

        if Self::pool_exists(&env, &user, &token0, &token1) {
            panic_with_error!(env, ContractError::AlreadyExists);
        }

        //get wasm hash from storage
        let wasm_hash = env
            .storage()
            .persistent()
            .get::<Symbol, BytesN<32>>(&LOBSTER_WASM_HASH)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        // the salt comes from token0, token1 and the user, so the vault address
        // follows from those three and the factory's own address

        let salt = {
            let mut salt_data = Bytes::new(&env);
            salt_data.append(&token0.clone().to_xdr(&env));
            salt_data.append(&token1.clone().to_xdr(&env));
            salt_data.append(&user.clone().to_xdr(&env));

            // Hash to get deterministic 32-byte salt
            env.crypto().sha256(&salt_data)
        };
        let fee_bps = env.storage().persistent().get(&FEES).unwrap_or(0u32);

        // trusted Soroswap router baked into the vault so it never trusts a
        // caller-supplied router on the add path
        let sw_router: Address = env
            .storage()
            .persistent()
            .get(&SW_ROUTER)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));

        let deploy_args: Vec<Val> = vec![
            &env,
            user.clone().into_val(&env),
            admin.into_val(&env),
            token0.into_val(&env),
            token1.into_val(&env),
            fee_bps.into_val(&env),
            sw_router.into_val(&env),
        ];

        let deployed_address = env
            .deployer()
            .with_address(env.current_contract_address(), salt)
            .deploy_v2(wasm_hash, deploy_args);

        let lobster_pool = LobsterPools {
            token0: token0.clone(),
            token1: token1.clone(),
            lobster_address: deployed_address.clone(),
            owner: user.clone(),
        };

        let lobster_pool_new_count = lobster_pool_count
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        lobster_user.push_back(lobster_pool.clone());

        lobster_token.push_back(lobster_pool.clone());

        env.storage()
            .persistent()
            .set(&LOBSTER_POOL_COUNT, &lobster_pool_new_count);

        env.storage().persistent().set(
            &(LOBSTER_POOLS_ID, lobster_pool_new_count.clone()),
            &lobster_pool.clone(),
        );

        env.storage().persistent().set(
            &(LOBSTER_POOLS_ADR, deployed_address.clone()),
            &lobster_pool.clone(),
        );

        env.storage()
            .persistent()
            .set(&(LOBSTER_POOL_USER, user.clone()), &lobster_user.clone());

        env.storage().persistent().set(
            &(LOBSTER_POOL_TOKEN, token0.clone(), token1.clone()),
            &lobster_token.clone(),
        );

        // Emit event for pool creation
        env.events().publish(
            (symbol_short!("pool_crt"), user.clone()),
            (
                deployed_address.clone(),
                token0.clone(),
                token1.clone(),
                lobster_pool_new_count,
            ),
        );

        Self::exit_reentrancy(&env);

        return deployed_address;
    }

    /// Checks if a pool already exists for the given user and token pair.
    /// Checks both (token0, token1) and (token1, token0) orderings.
    /// 
    /// # Parameters
    /// - `user`: Address of the user to check
    /// - `token0`: Address of the first token
    /// - `token1`: Address of the second token
    /// 
    /// # Returns
    /// `true` if a pool exists for this user and token pair, `false` otherwise
    fn pool_exists(env: &Env, user: &Address, token0: &Address, token1: &Address) -> bool {
        // Try both (token0, token1) and (token1, token0)
        let existing_a = env
            .storage()
            .persistent()
            .get::<(Symbol, Address, Address), Vec<LobsterPools>>(&(
                LOBSTER_POOL_TOKEN,
                token0.clone(),
                token1.clone(),
            ));

        let existing_b = env
            .storage()
            .persistent()
            .get::<(Symbol, Address, Address), Vec<LobsterPools>>(&(
                LOBSTER_POOL_TOKEN,
                token1.clone(),
                token0.clone(),
            ));

        // Check if any pool matches the user
        let found_a = existing_a.map_or(false, |v| v.iter().any(|p| p.owner == *user));

        let found_b = existing_b.map_or(false, |v| v.iter().any(|p| p.owner == *user));

        found_a || found_b
    }

    /// Returns the address of the factory admin.
    /// 
    /// # Returns
    /// The admin address stored during initialization
    /// 
    /// # Panics
    /// If contract is not initialized
    pub fn get_admin(env: Env) -> Address {
        let admin: Address = env
            .storage()
            .persistent()
            .get::<Symbol, Address>(&ADMIN)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        admin
    }

    /// Returns the WASM hash used for deploying new Lobster contracts.
    /// 
    /// # Returns
    /// The WASM hash (32 bytes) stored during initialization, or zero hash if not set
    pub fn get_wasm_hash(env: Env) -> BytesN<32> {
        // Prefer instance storage; fall back to default zeroed hash
        env.storage()
            .persistent()
            .get::<Symbol, BytesN<32>>(&LOBSTER_WASM_HASH)
            .unwrap_or_else(|| BytesN::from_array(&env, &[0; 32]))
    }

    /// Returns the total number of pools created by this factory.
    /// 
    /// # Returns
    /// Total count of pools created (starts at 0)
    pub fn get_pool_count(env: Env) -> u64 {
        let lobster_pool_count: u64 = env
            .storage()
            .persistent()
            .get::<Symbol, u64>(&LOBSTER_POOL_COUNT)
            .unwrap_or(0u64);
        lobster_pool_count
    }

    /// Retrieves pool information by its sequential ID.
    /// 
    /// # Parameters
    /// - `id`: Sequential pool ID (1-indexed)
    /// 
    /// # Returns
    /// `Some(LobsterPools)` if pool exists, `None` otherwise
    pub fn get_pool_by_id(env: Env, id: u64) -> Option<LobsterPools> {
        env.storage()
            .persistent()
            .get::<(Symbol, u64), LobsterPools>(&(LOBSTER_POOLS_ID, id))
    }

    /// Retrieves pool information by the Lobster contract address.
    /// 
    /// # Parameters
    /// - `addr`: Address of the Lobster contract
    /// 
    /// # Returns
    /// `Some(LobsterPools)` if pool exists, `None` otherwise
    pub fn get_pool_by_address(env: Env, addr: Address) -> Option<LobsterPools> {
        env.storage()
            .persistent()
            .get::<(Symbol, Address), LobsterPools>(&(LOBSTER_POOLS_ADR, addr))
    }

    /// Retrieves all pools owned by a specific user.
    /// 
    /// # Parameters
    /// - `user`: Address of the user/owner
    /// 
    /// # Returns
    /// Vector of all LobsterPools owned by the user (empty if none)
    pub fn get_pools_by_user(env: Env, user: Address) -> Vec<LobsterPools> {
        env.storage()
            .persistent()
            .get::<(Symbol, Address), Vec<LobsterPools>>(&(LOBSTER_POOL_USER, user))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Retrieves all pools for a specific token pair.
    /// Checks both (token0, token1) and (token1, token0) orderings.
    /// 
    /// # Parameters
    /// - `token0`: Address of the first token
    /// - `token1`: Address of the second token
    /// 
    /// # Returns
    /// Vector of all LobsterPools for this token pair (empty if none)
    pub fn get_pools_by_tokens(env: Env, token0: Address, token1: Address) -> Vec<LobsterPools> {
        let a = env
            .storage()
            .persistent()
            .get::<(Symbol, Address, Address), Vec<LobsterPools>>(&(
                LOBSTER_POOL_TOKEN,
                token0.clone(),
                token1.clone(),
            ))
            .unwrap_or_else(|| Vec::new(&env));
        let b = env
            .storage()
            .persistent()
            .get::<(Symbol, Address, Address), Vec<LobsterPools>>(&(
                LOBSTER_POOL_TOKEN,
                token1.clone(),
                token0.clone(),
            ))
            .unwrap_or_else(|| Vec::new(&env));
        // Concatenate results
        let mut out = Vec::new(&env);
        for p in a.iter() {
            out.push_back(p.clone());
        }
        for p in b.iter() {
            out.push_back(p.clone());
        }
        out
    }

    /// Updates the factory admin address.
    /// 
    /// # Parameters
    /// - `new_admin`: New admin address to set
    /// 
    /// # Access
    /// Only current admin
    /// 
    /// # Panics
    /// - If contract is not initialized
    /// - If caller is not the current admin
    pub fn set_admin(env: Env, new_admin: Address) {
        if !env.storage().persistent().has(&ADMIN) {
            panic_with_error!(env, ContractError::NotInitialized);
        }
        let current_admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        current_admin.require_auth();
        Self::extend_state_ttl(&env);
        env.storage().persistent().set(&ADMIN, &new_admin);
    }

    /// Updates the WASM hash used for deploying new Lobster contracts.
    /// 
    /// # Parameters
    /// - `wasm_hash`: New WASM hash (32 bytes) to use for deployments
    /// 
    /// # Access
    /// Only admin
    /// 
    /// # Panics
    /// - If contract is not initialized
    /// - If caller is not the admin
    pub fn set_wasm_hash(env: Env, wasm_hash: BytesN<32>) {
        if !env.storage().persistent().has(&ADMIN) {
            panic_with_error!(env, ContractError::NotInitialized);
        }
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        admin.require_auth();
        Self::extend_state_ttl(&env);
        env.storage()
            .persistent()
            .set(&LOBSTER_WASM_HASH, &wasm_hash);
    }

    // fee changes: announce_fee_change starts a 14-day notice, execute_fee_change applies it

    /// Announces a fee change, starting a 2-week delay period before execution.
    /// This provides transparency and allows users to prepare for fee changes.
    /// 
    /// # Parameters
    /// - `new_fee_bps`: new fee in basis points, capped at MAX_FEE_BPS (2000 = 20%)
    /// 
    /// # Access
    /// Only admin
    /// 
    /// # Panics
    /// - If contract is not initialized
    /// - If caller is not the admin
    /// - If new_fee_bps exceeds MAX_FEE_BPS
    /// - If a fee change is already pending
    pub fn announce_fee_change(env: Env, new_fee_bps: u32) {
        if !env.storage().persistent().has(&ADMIN) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        admin.require_auth();

        // fee is a cut of profit, capped at MAX_FEE_BPS
        if new_fee_bps > MAX_FEE_BPS {
            panic_with_error!(env, ContractError::InvalidFeeValue);
        }

        // Prevent overwriting an existing pending fee change
        if env.storage().persistent().has(&PENDING_FEE_CHANGE) {
            panic_with_error!(env, ContractError::FeeChangeAlreadyPending);
        }

        let current_timestamp = env.ledger().timestamp();
        let delay_period = 14 * 24 * 60 * 60; // 14 days in seconds
        let execution_timestamp = current_timestamp
            .checked_add(delay_period)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::ContractMathError));

        let pending_change = PendingFeeChange {
            new_fee_bps,
            announcement_timestamp: current_timestamp,
            execution_timestamp,
        };

        env.storage()
            .persistent()
            .set(&PENDING_FEE_CHANGE, &pending_change);

        // Emit event
        env.events().publish(
            (symbol_short!("fee_chge"), admin.clone()),
            (new_fee_bps, current_timestamp, execution_timestamp),
        );
    }

    /// Executes the previously announced fee change after the 2-week delay period.
    /// Updates the fee and clears the pending change.
    /// 
    /// # Access
    /// Only admin
    /// 
    /// # Panics
    /// - If contract is not initialized
    /// - If caller is not the admin
    /// - If no fee change has been announced
    /// - If the delay period has not yet passed
    pub fn execute_fee_change(env: Env) {
        if !env.storage().persistent().has(&ADMIN) {
            panic_with_error!(env, ContractError::NotInitialized);
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
        admin.require_auth();

        let pending_change: PendingFeeChange = env
            .storage()
            .persistent()
            .get(&PENDING_FEE_CHANGE)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::FeeChangeNotAnnounced));

        let current_timestamp = env.ledger().timestamp();

        if current_timestamp < pending_change.execution_timestamp {
            panic_with_error!(env, ContractError::FeeChangeNotReady);
        }

        env.storage()
            .persistent()
            .set(&FEES, &pending_change.new_fee_bps);

        // Clear the pending change
        env.storage().persistent().remove(&PENDING_FEE_CHANGE);

        // Emit event
        env.events().publish(
            (symbol_short!("fee_apply"), admin.clone()),
            pending_change.new_fee_bps,
        );
    }
}
