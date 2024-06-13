use num_enum::IntoPrimitive;
use solana_program::{entrypoint::ProgramResult, msg, program_error::ProgramError};
use thiserror::Error;

// todo: group error blocks by kind
// todo: add comments which indicate decimal code for an error
#[derive(Error, Debug, Clone, PartialEq, Eq, IntoPrimitive)]
#[repr(u32)]
pub enum MangoError {
    #[error("")]
    SomeError,
    #[error("")]
    NotImplementedError,
    #[error("checked math error")]
    MathError,
    #[error("")]
    UnexpectedOracle,
    #[error("oracle type cannot be determined")]
    UnknownOracleType,
    #[error("")]
    InvalidFlashLoanTargetCpiProgram,
    #[error("health must be positive")]
    HealthMustBePositive,
    #[error("health must be positive or not decrease")]
    HealthMustBePositiveOrIncrease, // outdated name is kept for backwards compatibility
    #[error("health must be negative")]
    HealthMustBeNegative,
    #[error("the account is bankrupt")]
    IsBankrupt,
    #[error("the account is not bankrupt")]
    IsNotBankrupt,
    #[error("no free token position index")]
    NoFreeTokenPositionIndex,
    #[error("no free serum3 open orders index")]
    NoFreeSerum3OpenOrdersIndex,
    #[error("no free perp position index")]
    NoFreePerpPositionIndex,
    #[error("serum3 open orders exist already")]
    Serum3OpenOrdersExistAlready,
    #[error("bank vault has insufficent funds")]
    InsufficentBankVaultFunds,
    #[error("account is currently being liquidated")]
    BeingLiquidated,
    #[error("invalid bank")]
    InvalidBank,
    #[error("account profitability is mismatched")]
    ProfitabilityMismatch,
    #[error("cannot settle with self")]
    CannotSettleWithSelf,
    #[error("perp position does not exist")]
    PerpPositionDoesNotExist,
    #[error("max settle amount must be greater than zero")]
    MaxSettleAmountMustBeGreaterThanZero,
    #[error("the perp position has open orders or unprocessed fill events")]
    HasOpenPerpOrders,
    #[error("an oracle does not reach the confidence threshold")]
    OracleConfidence,
    #[error("an oracle is stale")]
    OracleStale,
    #[error("settlement amount must always be positive")]
    SettlementAmountMustBePositive,
    #[error("bank utilization has reached limit")]
    BankBorrowLimitReached,
    #[error("bank net borrows has reached limit - this is an intermittent error - the limit will reset regularly")]
    BankNetBorrowsLimitReached,
    #[error("token position does not exist")]
    TokenPositionDoesNotExist,
    #[error("token deposits into accounts that are being liquidated must bring their health above the init threshold")]
    DepositsIntoLiquidatingMustRecover,
    #[error("token is in reduce only mode")]
    TokenInReduceOnlyMode,
    #[error("market is in reduce only mode")]
    MarketInReduceOnlyMode,
    #[error("group is halted")]
    GroupIsHalted,
    #[error("the perp position has non-zero base lots")]
    PerpHasBaseLots,
    #[error("there are open or unsettled spot orders")]
    HasOpenOrUnsettledSpotOrders,
    #[error("has liquidatable token position")]
    HasLiquidatableTokenPosition,
    #[error("has liquidatable perp base position")]
    HasLiquidatablePerpBasePosition,
    #[error("has liquidatable positive perp pnl")]
    HasLiquidatablePositivePerpPnl,
    #[error("account is frozen")]
    AccountIsFrozen,
    #[error("Init Asset Weight can't be negative")]
    InitAssetWeightCantBeNegative,
    #[error("has open perp taker fills")]
    HasOpenPerpTakerFills,
    #[error("deposit crosses the current group deposit limit")]
    DepositLimit,
    #[error("instruction is disabled")]
    IxIsDisabled,
    #[error("no liquidatable perp base position")]
    NoLiquidatablePerpBasePosition,
    #[error("perp order id not found on the orderbook")]
    PerpOrderIdNotFound,
    #[error("HealthRegions allow only specific instructions between Begin and End")]
    HealthRegionBadInnerInstruction,
    #[error("token is in force close")]
    TokenInForceClose,
    #[error("incorrect number of health accounts")]
    InvalidHealthAccountCount,
    #[error("would self trade")]
    WouldSelfTrade,
    #[error("token conditional swap oracle price is not in execution range")]
    TokenConditionalSwapPriceNotInRange,
    #[error("token conditional swap is expired")]
    TokenConditionalSwapExpired,
    #[error("token conditional swap is not available yet")]
    TokenConditionalSwapNotStarted,
    #[error("token conditional swap was already started")]
    TokenConditionalSwapAlreadyStarted,
    #[error("token conditional swap it not set")]
    TokenConditionalSwapNotSet,
    #[error("token conditional swap trigger did not reach min_buy_token")]
    TokenConditionalSwapMinBuyTokenNotReached,
    #[error("token conditional swap cannot pay incentive")]
    TokenConditionalSwapCantPayIncentive,
    #[error("token conditional swap taker price is too low")]
    TokenConditionalSwapTakerPriceTooLow,
    #[error("token conditional swap index and id don't match")]
    TokenConditionalSwapIndexIdMismatch,
    #[error("token conditional swap volume is too small compared to the cost of starting it")]
    TokenConditionalSwapTooSmallForStartIncentive,
    #[error("token conditional swap type cannot be started")]
    TokenConditionalSwapTypeNotStartable,
    #[error("a bank in the health account list should be writable but is not")]
    HealthAccountBankNotWritable,
    #[error("the market does not allow limit orders too far from the current oracle value")]
    SpotPriceBandExceeded,
    #[error("deposit crosses the token's deposit limit")]
    BankDepositLimit,
    #[error("delegates can only withdraw to the owner's associated token account")]
    DelegateWithdrawOnlyToOwnerAta,
    #[error("delegates can only withdraw if they close the token position")]
    DelegateWithdrawMustClosePosition,
    #[error("delegates can only withdraw small amounts")]
    DelegateWithdrawSmall,
    #[error("The provided CLMM oracle is not valid")]
    InvalidCLMMOracle,
    #[error("invalid usdc/usd feed provided for the CLMM oracle")]
    InvalidFeedForCLMMOracle,
    #[error("Pyth USDC/USD or SOL/USD feed not found (required by CLMM oracle)")]
    MissingFeedForCLMMOracle,
    #[error("the asset does not allow liquidation")]
    TokenAssetLiquidationDisabled,
    #[error("for borrows the bank must be in the health account list")]
    BorrowsRequireHealthAccountBank,
    #[error("invalid sequence number")]
    InvalidSequenceNumber,
    #[error("invalid health")]
    InvalidHealth,
    #[error("no free openbook v2 open orders index")]
    NoFreeOpenbookV2OpenOrdersIndex,
    #[error("openbook v2 open orders exist already")]
    OpenbookV2OpenOrdersExistAlready,
}

impl From<MangoError> for ProgramError {
    fn from(e: MangoError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

#[track_caller]
#[inline(always)]
pub fn assert_with_msg(v: bool, err: impl Into<ProgramError>, msg: &str) -> ProgramResult {
    if v {
        Ok(())
    } else {
        let caller = std::panic::Location::caller();
        msg!("{}. \n{}", msg, caller);
        Err(err.into())
    }
}
