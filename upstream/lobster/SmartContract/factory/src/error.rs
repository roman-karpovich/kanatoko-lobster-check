use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AlreadyExists = 3,
    PoolAlreadyExists = 4,
    PoolNotFound = 5,
    PoolNotOwned = 6,
    PoolNotActive = 7,
    PoolNotValid = 8,
    InitializeFailed = 9,
    FeeChangeNotAnnounced = 10,
    FeeChangeNotReady = 11,
    InvalidFeeValue = 12,
    FeeChangeAlreadyPending = 13,
    InvalidPoolTokens = 14,
    ContractMathError = 15,
}
