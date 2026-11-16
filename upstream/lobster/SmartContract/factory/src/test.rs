#[cfg(test)]
mod tests {
    use crate::{Factory, FactoryClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        token, Address, BytesN, Env, Vec, Val, vec, IntoVal
    };
    
    // Import Lobster contract for WASM
    mod lobster {
        soroban_sdk::contractimport!(
            file = "../target/wasm32v1-none/release/lobster_contract.wasm"
        );
    }

    // Helper functions for creating contracts
    pub fn create_token_contract<'a>(
        e: &Env,
        admin: &Address,
    ) -> (token::Client<'a>, token::StellarAssetClient<'a>) {
        let sac = e.register_stellar_asset_contract_v2(admin.clone());
        (
            token::Client::new(e, &sac.address()),
            token::StellarAssetClient::new(e, &sac.address()),
        )
    }

    fn install_lobster_wasm(env: &Env) -> BytesN<32> {
        env.deployer().upload_contract_wasm(lobster::WASM)
    }

    fn create_test_env() -> Env {
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
        env
    }

    #[test]
    fn test_initialize() {
        let env = create_test_env();
        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32; // 1% fee

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        // Verify admin is set
        let stored_admin = factory.get_admin();
        assert_eq!(stored_admin, admin);

        // Verify wasm hash is set
        let stored_hash = factory.get_wasm_hash();
        assert_eq!(stored_hash, wasm_hash);

        // // Verify pool count is 0
        let pool_count = factory.get_pool_count();
        assert_eq!(pool_count, 0u64);
    }


    #[test]
    fn test_create_pool() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let token_admin = Address::generate(&env);

        // Create tokens
        let (token0, _token0_admin) = create_token_contract(&env, &token_admin);
        let (token1, _token1_admin) = create_token_contract(&env, &token_admin);

        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let stored_hash: BytesN<32> = factory.get_wasm_hash();
        assert_eq!(stored_hash, wasm_hash);

        factory.create_pool(&user, &token0.address, &token1.address);

        // Verify pool count increased
        let pool_count = factory.get_pool_count();
        assert_eq!(pool_count, 1);

        // Verify pool is retrievable by ID
        let pool = factory.get_pool_by_id(&1u64).unwrap();
        assert_eq!(pool.owner, user);
        assert_eq!(pool.token0, token0.address);
        assert_eq!(pool.token1, token1.address);

        // Verify pool is retrievable by address
        let pool_by_addr = factory.get_pool_by_address(&pool.lobster_address).unwrap();
        assert_eq!(pool_by_addr.owner, user);

        // Verify pool is in user's pools
        let user_pools = factory.get_pools_by_user(&user);
        assert_eq!(user_pools.len(), 1);
        assert_eq!(
            user_pools.get(0).unwrap().lobster_address,
            pool.lobster_address
        );

        // Verify pool is in token pair pools
        let token_pools = factory.get_pools_by_tokens(&token0.address, &token1.address);
        assert_eq!(token_pools.len(), 1);
    }

    #[test]
    fn test_create_multiple_pools() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let user1 = Address::generate(&env);
        let user2 = Address::generate(&env);
        let token_admin = Address::generate(&env);

        let (token0, _token0_admin) = create_token_contract(&env, &token_admin);
        let (token1, _token1_admin) = create_token_contract(&env, &token_admin);
        let (token2, _token2_admin) = create_token_contract(&env, &token_admin);

        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        // Create pool 1: user1, token0-token1
        factory.create_pool(&user1, &token0.address, &token1.address);

        // Create pool 2: user2, token0-token2
        factory.create_pool(&user2, &token0.address, &token2.address);

        // Create pool 3: user1, token1-token2
        factory.create_pool(&user1, &token1.address, &token2.address);

        assert_eq!(factory.get_pool_count(), 3);

        // Verify user1 has 2 pools
        let user1_pools = factory.get_pools_by_user(&user1);
        assert_eq!(user1_pools.len(), 2);

        // Verify user2 has 1 pool
        let user2_pools = factory.get_pools_by_user(&user2);
        assert_eq!(user2_pools.len(), 1);

        // Verify token0-token1 pools
        let pools_token0_token1 = factory.get_pools_by_tokens(&token0.address, &token1.address);
        assert_eq!(pools_token0_token1.len(), 1);

        // Verify token1-token2 pools
        let pools_token1_token2 = factory.get_pools_by_tokens(&token1.address, &token2.address);
        assert_eq!(pools_token1_token2.len(), 1);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #3)")]
    fn test_create_duplicate_pool() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let token_admin = Address::generate(&env);

        let (token0, _token0_admin) = create_token_contract(&env, &token_admin);
        let (token1, _token1_admin) = create_token_contract(&env, &token_admin);

        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        // Create the same pool twice (same user, same tokens)
        factory.create_pool(&user, &token0.address, &token1.address);
        factory.create_pool(&user, &token0.address, &token1.address);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #3)")]
    fn test_pool_exists_with_reversed_tokens() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let token_admin = Address::generate(&env);

        let (token0, _token0_admin) = create_token_contract(&env, &token_admin);
        let (token1, _token1_admin) = create_token_contract(&env, &token_admin);

        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        // Create pool with token0-token1
        factory.create_pool(&user, &token0.address, &token1.address);

        // Try to create pool with reversed tokens (should fail)

        factory.create_pool(&user, &token1.address, &token0.address);
    }

    #[test]
    fn test_set_admin() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        factory.set_admin(&new_admin);

        let current_admin = factory.get_admin();
        assert_eq!(current_admin, new_admin);
    }

    #[test]
    fn test_set_wasm_hash() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let new_wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);
        factory.set_wasm_hash(&new_wasm_hash);

        let stored_hash = factory.get_wasm_hash();
        assert_eq!(stored_hash, new_wasm_hash);
    }

    #[test]
    fn test_announce_fee_change() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let new_fee_bps = 200u32; // 2% fee
        factory.announce_fee_change(&new_fee_bps);

        // The announcement is made, but execution requires time delay
        // We can't easily test events in current SDK, so just verify no panic
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #12)")]
    fn test_announce_fee_change_invalid() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let invalid_fee_bps = 10001u32; // > 10000 (100%)
        factory.announce_fee_change(&invalid_fee_bps);
    }

    #[test]
    fn test_execute_fee_change() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let new_fee_bps = 200u32;

        // Announce fee change
        factory.announce_fee_change(&new_fee_bps);

        // Advance time 
        env.ledger().with_mut(|li| {
            li.timestamp = 12345 + (14 * 24 * 60 * 60) +1 ;
        });

        // Execute fee change
        factory.execute_fee_change();
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #11)")]
    fn test_execute_fee_change_too_early() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let new_fee_bps = 200u32;

        // Announce fee change
        factory.announce_fee_change(&new_fee_bps);

        // Try to execute immediately (should fail)
        factory.execute_fee_change();
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #10)")]
    fn test_execute_fee_change_not_announced() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        // Try to execute without announcing
        factory.execute_fee_change();
    }

    #[test]
    fn test_get_pool_by_id_nonexistent() {
        let env = create_test_env();

        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let pool = factory.get_pool_by_id(&999u64);
        assert!(pool.is_none());
    }

    #[test]
    fn test_get_pool_by_address_nonexistent() {
        let env = create_test_env();

        let admin = Address::generate(&env);
        let fake_address = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let pool = factory.get_pool_by_address(&fake_address);
        assert!(pool.is_none());
    }

    #[test]
    fn test_get_pools_by_user_empty() {
        let env = create_test_env();

        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let pools = factory.get_pools_by_user(&user);
        assert_eq!(pools.len(), 0);
    }

    #[test]
    fn test_get_pools_by_tokens_empty() {
        let env = create_test_env();

        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let (token0, _token0_admin) = create_token_contract(&env, &token_admin);
        let (token1, _token1_admin) = create_token_contract(&env, &token_admin);

        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        let pools = factory.get_pools_by_tokens(&token0.address, &token1.address);
        assert_eq!(pools.len(), 0);
    }

    #[test]
    fn test_get_pools_by_tokens_symmetric() {
        let env = create_test_env();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let token_admin = Address::generate(&env);

        let (token0, _token0_admin) = create_token_contract(&env, &token_admin);
        let (token1, _token1_admin) = create_token_contract(&env, &token_admin);

        let wasm_hash = install_lobster_wasm(&env);
        let fee_bps = 100u32;

        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            fee_bps.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];

        let factory_id = env.register(Factory, deploy_args);
        let factory = FactoryClient::new(&env, &factory_id);

        factory.create_pool(&user, &token0.address, &token1.address);

        // Get pools using same order
        let pools1 = factory.get_pools_by_tokens(&token0.address, &token1.address);
        assert_eq!(pools1.len(), 1);

        // Get pools using reversed order - should still find it
        let pools2 = factory.get_pools_by_tokens(&token1.address, &token0.address);
        assert_eq!(pools2.len(), 1);
    }

    // the factory caps the fee it bakes into vaults at 20% (2000 bps).
    #[test]
    #[should_panic]
    fn test_constructor_rejects_fee_above_cap() {
        let env = create_test_env();
        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let deploy_args: Vec<Val> = vec![
            &env,
            admin.into_val(&env),
            wasm_hash.into_val(&env),
            2001u32.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];
        env.register(Factory, deploy_args);
    }

    // control: a fee exactly at the cap constructs cleanly.
    #[test]
    fn test_constructor_accepts_fee_at_cap() {
        let env = create_test_env();
        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let deploy_args: Vec<Val> = vec![
            &env,
            admin.clone().into_val(&env),
            wasm_hash.into_val(&env),
            2000u32.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];
        let factory_id = env.register(Factory, deploy_args);
        assert_eq!(FactoryClient::new(&env, &factory_id).get_admin(), admin);
    }

    // an announced fee change is capped the same way.
    #[test]
    #[should_panic]
    fn test_announce_fee_change_rejects_above_cap() {
        let env = create_test_env();
        let admin = Address::generate(&env);
        let wasm_hash = install_lobster_wasm(&env);
        let deploy_args: Vec<Val> = vec![
            &env,
            admin.into_val(&env),
            wasm_hash.into_val(&env),
            100u32.into_val(&env),
            Address::generate(&env).into_val(&env),
        ];
        let factory = FactoryClient::new(&env, &env.register(Factory, deploy_args));
        factory.announce_fee_change(&2001u32);
    }
}
