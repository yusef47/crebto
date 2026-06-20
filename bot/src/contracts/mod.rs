use alloy::sol;

sol! {
    #[sol(rpc)]
    interface IMEVExecutor {
        function executeLiquidationBalancer(
            address aavePool,
            address collateral,
            address debt,
            address user,
            uint256 debtToCover,
            uint256 flashAmount,
            uint256 minAmountOut
        ) external;
    }

    #[sol(rpc)]
    interface IAavePool {
        function getUserAccountData(address user) external view returns (
            uint256 totalCollateralBase,
            uint256 totalDebtBase,
            uint256 availableBorrowsBase,
            uint256 currentLiquidationThreshold,
            uint256 ltv,
            uint256 healthFactor
        );

        function getUserReserveData(address asset, address user) external view returns (
            uint256 currentATokenBalance,
            uint256 currentStableDebt,
            uint256 currentVariableDebt,
            uint256 principalStableDebt,
            uint256 scaledVariableDebt,
            uint256 stableBorrowRate,
            uint256 liquidityRate,
            uint40 stableRateLastUpdated,
            bool usageAsCollateralEnabled
        );

        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;
    }

    #[sol(rpc)]
    interface IBalancerVault {
        function flashLoan(
            address recipient,
            address[] memory tokens,
            uint256[] memory amounts,
            bytes memory userData
        ) external;
    }
}

