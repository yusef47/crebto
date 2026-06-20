// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/**
 * @title MEVExecutor
 * @notice Atomic flash-loan executor for Aave V3 liquidations on Base L2.
 * @dev Uses Balancer V2 for 0% fee flash loans. All profit swept to owner.
 *      No block.coinbase bribes — MEV-Share refund handled off-chain.
 */
contract MEVExecutor is ReentrancyGuard, Ownable {
    using SafeERC20 for IERC20;

    // ─── Balancer V2 Flash Loan Interface ───
    interface IBalancerVault {
        function flashLoan(
            address recipient,
            address[] memory tokens,
            uint256[] memory amounts,
            bytes memory userData
        ) external;
    }

    // ─── Aave V3 Pool Interface ───
    interface IAavePool {
        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;
    }

    // ─── Aerodrome Router Interface ───
    interface IAerodromeRouter {
        struct Route {
            address from;
            address to;
            bool stable;
            address factory;
        }

        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            Route[] calldata routes,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);
    }

    // ─── Events ───
    event LiquidationExecuted(
        address indexed user,
        address indexed collateral,
        address indexed debt,
        uint256 debtRepaid,
        uint256 profit
    );
    event ProfitSwept(uint256 amount);
    event OwnerSweep(address indexed token, uint256 amount);
    event FlashLoanRepaid(address indexed token, uint256 amount);

    // ─── Immutable Constants ───
    IBalancerVault public constant BALANCER_VAULT =
        IBalancerVault(0xBA12222222228d8Ba445958a75A0704d566BF2C8);

    IAerodromeRouter public constant AERODROME_ROUTER =
        IAerodromeRouter(0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43);

    address public immutable WETH;

    // ─── State ───
    mapping(address => bool) public authorizedSearchers;

    modifier onlyAuthorized() {
        require(
            msg.sender == owner() || authorizedSearchers[msg.sender],
            "UNAUTHORIZED"
        );
        _;
    }

    constructor(address _weth) Ownable(msg.sender) {
        WETH = _weth;
    }

    // ─── Admin ───
    function setSearcher(address searcher, bool authorized) external onlyOwner {
        authorizedSearchers[searcher] = authorized;
    }

    /// @notice Sweep any ERC20 stuck in the contract (emergency rescue).
    function sweepToOwner(address token, uint256 amount) external onlyOwner {
        IERC20(token).safeTransfer(owner(), amount);
        emit OwnerSweep(token, amount);
    }

    /// @notice Sweep native ETH stuck in the contract.
    function rescueETH() external onlyOwner {
        payable(owner()).transfer(address(this).balance);
    }

    // ═══════════════════════════════════════════════════════
    //  MAIN ENTRY: Liquidation via Balancer Flash Loan
    // ═══════════════════════════════════════════════════════

    /**
     * @notice Flash-loan repayment asset, liquidate underwater Aave position.
     * @param aavePool      Aave V3 Pool address on Base.
     * @param collateral    Collateral token to seize.
     * @param debt          Debt token to repay (flash-loaned from Balancer).
     * @param user          Underwater borrower.
     * @param debtToCover   Amount of debt to repay (<= flashAmount).
     * @param flashAmount   Exact amount of `debt` to flash loan from Balancer.
     * @param minAmountOut  Minimum collateral→debt output (slippage protection).
     *                      Pass 0 if you accept any output (Phase 0 default).
     */
    function executeLiquidationBalancer(
        address aavePool,
        address collateral,
        address debt,
        address user,
        uint256 debtToCover,
        uint256 flashAmount,
        uint256 minAmountOut
    ) external onlyAuthorized {
        require(debtToCover <= flashAmount, "DEBT_GT_FLASH");

        address[] memory tokens = new address[](1);
        uint256[] memory amounts = new uint256[](1);
        tokens[0] = debt;
        amounts[0] = flashAmount;

        bytes memory userData = abi.encode(
            aavePool,
            collateral,
            debt,
            user,
            debtToCover,
            minAmountOut
        );

        BALANCER_VAULT.flashLoan(address(this), tokens, amounts, userData);
    }

    // ═══════════════════════════════════════════════════════
    //  BALANCER FLASH LOAN CALLBACK
    // ═══════════════════════════════════════════════════════

    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external nonReentrant {
        require(msg.sender == address(BALANCER_VAULT), "INVALID_CALLER");

        (
            address aavePool,
            address collateral,
            address debt,
            address user,
            uint256 debtToCover,
            uint256 minAmountOut
        ) = abi.decode(userData, (address, address, address, address, uint256, uint256));

        // Approve Aave Pool to pull repayment
        IERC20(debt).safeIncreaseAllowance(aavePool, debtToCover);

        // Track collateral balance before liquidation for precise swap amount
        uint256 collateralBefore = IERC20(collateral).balanceOf(address(this));

        // Execute liquidation — seize collateral from underwater borrower
        IAavePool(aavePool).liquidationCall(
            collateral,
            debt,
            user,
            debtToCover,
            false // receive raw collateral, not aTokens
        );

        // ─── Aerodrome: swap seized collateral → debt token ───
        if (collateral != debt) {
            uint256 seizedCollateral = IERC20(collateral).balanceOf(address(this)) - collateralBefore;
            if (seizedCollateral > 0) {
                IERC20(collateral).safeIncreaseAllowance(address(AERODROME_ROUTER), seizedCollateral);

                IAerodromeRouter.Route[] memory routes = new IAerodromeRouter.Route[](1);
                routes[0] = IAerodromeRouter.Route({
                    from: collateral,
                    to: debt,
                    stable: false,
                    factory: address(0) // use Aerodrome default factory
                });

                IAerodromeRouter(AERODROME_ROUTER).swapExactTokensForTokens(
                    seizedCollateral,
                    minAmountOut,
                    routes,
                    address(this),
                    block.timestamp
                );
            }
        }

        // Repay Balancer (principal + fee)
        uint256 totalRepay = amounts[0] + feeAmounts[0];
        uint256 balance = IERC20(debt).balanceOf(address(this));
        require(balance >= totalRepay, "INSUFFICIENT_REPAY");

        IERC20(debt).safeTransfer(address(BALANCER_VAULT), totalRepay);
        emit FlashLoanRepaid(debt, totalRepay);

        // Calculate and emit profit
        uint256 remaining = IERC20(debt).balanceOf(address(this));
        uint256 profit = remaining >= totalRepay ? remaining - totalRepay : 0;
        if (remaining > 0) {
            _sweepProfitToOwner(debt, remaining);
        }
        emit LiquidationExecuted(user, collateral, debt, debtToCover, profit);
    }

    // ═══════════════════════════════════════════════════════
    //  INTERNAL: PROFIT SWEEP (NO BUILDER BRIBE)
    // ═══════════════════════════════════════════════════════

    function _sweepProfitToOwner(address token, uint256 amount) internal {
        if (amount == 0) return;
        IERC20(token).safeTransfer(owner(), amount);
        emit ProfitSwept(amount);
    }

    receive() external payable {}
}
