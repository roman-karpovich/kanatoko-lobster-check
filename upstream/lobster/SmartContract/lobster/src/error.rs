use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    NotAuthorized = 401,
    ContractMathError = 328,

    TokenTransferFailed = 330,

    /// SoroswapRouter: insufficient a amount
    InsufficientAAmount = 405,

    /// SoroswapRouter: insufficient b amount
    InsufficientBAmount = 406,


    // Cross-contract call failures (500+)
    QueryPoolInfoFailed = 500,

    PoolToken0QueryFailed = 510,
    RouterQuoteFailed = 512,
    RouterAddLiquidityFailed = 513,
    RouterRemoveLiquidityFailed = 514,



    LpTokenBalanceQueryFailed = 540,
    PoolLpBalanceQueryFailed = 541,
    RouterPairForFailed = 542,
    
    // TVL calculation errors
    InvalidProtocol = 550,
    ZeroReserves = 551,
    ActivePositionExists = 552,

    AlreadyInitialized = 560,
    InvalidFeeValue = 561,
    InvalidAmount = 562,
    NotInitialized = 563,
    ReentrancyDetected = 564,

    // withdraw must target the pool the vault opened, not an arbitrary one
    PoolMismatch = 565,
    // withdraw/close called with no open position on that protocol
    NoActivePosition = 566,
    // a pair must be two distinct tokens
    IdenticalTokens = 567,
    // router is not the trusted one this vault was configured with
    UntrustedDex = 568,
}
