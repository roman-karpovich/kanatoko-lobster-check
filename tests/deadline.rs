use kanatoko::soroban_env_host::xdr::ScError;
use kanatoko::soroban_sdk::{testutils::Ledger as _, token::TokenClient, Address};
use kanatoko::{mainnet, AutoRunner, ScenarioFork};

const ROUTER: &str = "CAG5LRYQ5JVEUI5TEID72EYOVX44TTUJT5BQR2J6J77FH65PCCFAJDDH";
const XLM: &str = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";
const USDC: &str = "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75";
const XLM_AMOUNT: i128 = 100_000_000;

fn wasm() -> &'static [u8] {
    if std::env::var_os("LOBSTER_MAINNET_WASM").is_some() {
        include_bytes!("../wasm/lobster_mainnet.wasm")
    } else {
        include_bytes!("../wasm/lobster_contract.wasm")
    }
}

fn runner(name: &str) -> AutoRunner {
    let path = format!("{}/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let runner = mainnet().cache(path);
    match std::env::var("KANATOKO_CAPTURE_RPC") {
        Ok(url) => runner.rpc_url(url).rpc_rate_limit(4).refresh(),
        Err(_) => runner.offline(),
    }
}

struct Position {
    owner: Address,
    vault: Address,
    pool: Address,
    xlm: Address,
    usdc: Address,
    router: Address,
    usdc_amount: i128,
}

fn setup(fork: &ScenarioFork<'_>) -> Position {
    fork.mock_all_auths();
    let env = fork.env();
    let owner = fork.local_account("lobster-owner");
    let admin = fork.local_account("lobster-admin");
    fork.fund_local_account(&owner, 10_000_000_000);
    let xlm = fork.contract(XLM);
    let usdc = fork.contract(USDC);
    let router = fork.contract(ROUTER);
    fork.invoke::<()>(&usdc, "trust", (&owner,));
    fork.invoke::<()>(&usdc, "set_authorized", (&owner, true));
    let pool: Address = fork.invoke(&router, "router_pair_for", (&xlm, &usdc));
    let token0: Address = fork.invoke(&pool, "token_0", ());
    let (r0, r1): (i128, i128) = fork.invoke(&pool, "get_reserves", ());
    let (xlm_reserve, usdc_reserve) = if token0 == xlm { (r0, r1) } else { (r1, r0) };
    let usdc_amount = XLM_AMOUNT * usdc_reserve / xlm_reserve + 1;
    fork.invoke::<()>(&usdc, "mint", (&owner, usdc_amount));
    let vault = fork.deploy(wasm(), (&owner, &admin, &xlm, &usdc, 100_u32, &router));
    // A local candidate must not inherit network token balances.
    assert_eq!(TokenClient::new(env, &xlm).balance(&vault), 0);
    assert_eq!(TokenClient::new(env, &usdc).balance(&vault), 0);
    fork.invoke::<()>(&vault, "deposit", (&owner, XLM_AMOUNT, usdc_amount));
    Position {
        owner,
        vault,
        pool,
        xlm,
        usdc,
        router,
        usdc_amount,
    }
}

fn balances(fork: &ScenarioFork<'_>, p: &Position) -> [i128; 5] {
    let xlm = TokenClient::new(fork.env(), &p.xlm);
    let usdc = TokenClient::new(fork.env(), &p.usdc);
    [
        xlm.balance(&p.vault),
        usdc.balance(&p.vault),
        xlm.balance(&p.pool),
        usdc.balance(&p.pool),
        TokenClient::new(fork.env(), &p.pool).balance(&p.vault),
    ]
}

fn add(
    fork: &ScenarioFork<'_>,
    p: &Position,
    deadline: u64,
) -> Result<(), kanatoko::InvocationFailure> {
    fork.try_invoke(
        &p.vault,
        "add_liquidity_soroswap",
        (
            &p.owner,
            XLM_AMOUNT,
            p.usdc_amount,
            0_i128,
            0_i128,
            deadline,
            &p.router,
        ),
    )
}

fn withdraw(
    fork: &ScenarioFork<'_>,
    p: &Position,
    deadline: u64,
) -> Result<(), kanatoko::InvocationFailure> {
    fork.try_invoke(
        &p.vault,
        "withdraw_soroswap",
        (&p.owner, 0_i128, 0_i128, &p.pool, &p.router, deadline),
    )
}

#[test]
fn add_deadlines_use_timestamp_and_rejections_preserve_balances() {
    runner("add")
        .run(|fork| {
            let p = setup(fork);
            let now = fork.env().ledger().timestamp();
            let ledger_deadline = u64::from(fork.env().ledger().sequence()) + 60;
            assert!(ledger_deadline < now);
            let before = balances(fork, &p);
            for deadline in [ledger_deadline, now - 1, now] {
                let failure = add(fork, &p, deadline).unwrap_err();
                assert_eq!(failure.error, Some(ScError::Contract(513)));
                assert_eq!(balances(fork, &p), before);
                assert_eq!(fork.invoke::<i128>(&p.vault, "get_active_protocol", ()), 5);
            }
            add(fork, &p, now + 60).unwrap();
            assert!(TokenClient::new(fork.env(), &p.pool).balance(&p.vault) > 0);
            assert_eq!(fork.invoke::<i128>(&p.vault, "get_active_protocol", ()), 0);
            assert_eq!(
                fork.invoke::<Address>(&p.vault, "get_actual_pool", ()),
                p.pool
            );
        })
        .unwrap();
}

#[test]
fn withdrawal_deadlines_use_timestamp_and_expire_at_equality() {
    runner("withdraw")
        .run(|fork| {
            let p = setup(fork);
            let now = fork.env().ledger().timestamp();
            add(fork, &p, now + 60).unwrap();
            let before = balances(fork, &p);
            let ledger_deadline = u64::from(fork.env().ledger().sequence()) + 60;
            for deadline in [ledger_deadline, now - 1, now] {
                let failure = withdraw(fork, &p, deadline).unwrap_err();
                assert_eq!(failure.error, Some(ScError::Contract(514)));
                assert_eq!(balances(fork, &p), before);
                assert_eq!(fork.invoke::<i128>(&p.vault, "get_active_protocol", ()), 0);
                assert_eq!(
                    fork.invoke::<Address>(&p.vault, "get_actual_pool", ()),
                    p.pool
                );
            }
            fork.env().ledger().set_timestamp(now + 60);
            assert_eq!(
                withdraw(fork, &p, now + 60).unwrap_err().error,
                Some(ScError::Contract(514))
            );
            assert_eq!(balances(fork, &p), before);
            withdraw(fork, &p, now + 61).unwrap();
            assert_eq!(TokenClient::new(fork.env(), &p.pool).balance(&p.vault), 0);
            assert!(TokenClient::new(fork.env(), &p.xlm).balance(&p.vault) > 0);
            assert!(TokenClient::new(fork.env(), &p.usdc).balance(&p.vault) > 0);
            assert_eq!(fork.invoke::<i128>(&p.vault, "get_active_protocol", ()), 5);
        })
        .unwrap();
}
