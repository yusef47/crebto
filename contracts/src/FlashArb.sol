// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title IFlashLoanSimpleReceiver
 * @notice Interface for Aave V3 Flash Loan callback receiver.
 */
interface IFlashLoanSimpleReceiver {
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool);
}

/**
 * @title IPool
 * @notice Minimal interface for Aave V3 Pool.
 */
interface IPool {
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;
}

/**
 * @title IERC20
 * @notice Minimal ERC20 interface.
 */
interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 value) external returns (bool);
    function approve(address spender, uint256 value) external returns (bool);
}

/**
 * @title FlashArb
 * @notice Blind execution contract for Aave V3 Flash Loans and Arbitrary Swaps on Base.
 */
contract FlashArb is IFlashLoanSimpleReceiver {
    address public immutable owner;
    address public immutable pool;
    mapping(address => bool) public allowedTargets;

    struct SwapStep {
        address target;   // The DEX router or pool contract to call
        bytes callData;   // The encoded swap call data
    }

    // Custom Errors for Gas Optimization
    error OnlyOwner();
    error OnlyPool();
    error ExecutionFailed();
    error InsufficientProfit();
    error InvalidInitiator();
    error TargetNotAllowed(address target);
    error TokenTransferFailed();

    event ArbitrageExecuted(address indexed asset, uint256 profit);
    event TargetAllowed(address indexed target, bool allowed);

    modifier onlyOwner() {
        if (msg.sender != owner) revert OnlyOwner();
        _;
    }

    modifier onlyPool() {
        if (msg.sender != pool) revert OnlyPool();
        _;
    }

    constructor(address _pool) {
        owner = msg.sender;
        pool = _pool;
    }

    /**
     * @notice Initiates the flash loan and arbitrage process.
     * @param asset The address of the token to borrow (e.g. USDC, WETH)
     * @param amount The amount to borrow
     * @param minProfit The minimum net profit required after repaying the flash loan
     * @param swapSteps The arbitrary execution steps for the swaps
     */
    function executeArbitrage(
        address asset,
        uint256 amount,
        uint256 minProfit,
        SwapStep[] calldata swapSteps
    ) external onlyOwner {
        bytes memory params = abi.encode(minProfit, swapSteps);
        
        // Initiate Flash Loan on Aave V3 Pool
        IPool(pool).flashLoanSimple(
            address(this),
            asset,
            amount,
            params,
            0
        );
    }

    /**
     * @notice Allows or blocks a swap target. Keep this list limited to trusted routers/pools.
     */
    function setTargetAllowed(address target, bool allowed) external onlyOwner {
        allowedTargets[target] = allowed;
        emit TargetAllowed(target, allowed);
    }

    /**
     * @notice Callback invoked by Aave Pool after sending the borrowed funds.
     */
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external override onlyPool returns (bool) {
        if (initiator != address(this)) revert InvalidInitiator();

        // Decode the swap steps
        (uint256 minProfit, SwapStep[] memory swapSteps) = abi.decode(params, (uint256, SwapStep[]));

        // Execute each swap step (Arbitrary execution)
        uint256 stepsLength = swapSteps.length;
        for (uint256 i = 0; i < stepsLength; ) {
            if (!allowedTargets[swapSteps[i].target]) revert TargetNotAllowed(swapSteps[i].target);
            (bool success, ) = swapSteps[i].target.call(swapSteps[i].callData);
            if (!success) revert ExecutionFailed();
            
            unchecked {
                i++;
            }
        }

        // Repayment amount
        uint256 amountToRepay = amount + premium;

        // Ensure we have enough to repay Aave
        uint256 currentBalance = IERC20(asset).balanceOf(address(this));
        if (currentBalance < amountToRepay + minProfit) revert InsufficientProfit();

        // Approve Pool to pull the repayment amount
        if (!IERC20(asset).approve(pool, amountToRepay)) revert TokenTransferFailed();

        // Send remaining profit to owner
        uint256 profit = currentBalance - amountToRepay;
        if (profit > 0) {
            if (!IERC20(asset).transfer(owner, profit)) revert TokenTransferFailed();
            emit ArbitrageExecuted(asset, profit);
        }

        return true;
    }

    /**
     * @notice Rescue stuck tokens in case of emergency.
     */
    function rescueTokens(address token) external onlyOwner {
        uint256 balance = IERC20(token).balanceOf(address(this));
        if (!IERC20(token).transfer(owner, balance)) revert TokenTransferFailed();
    }

    /**
     * @notice Rescue stuck ETH.
     */
    function rescueETH() external onlyOwner {
        payable(owner).transfer(address(this).balance);
    }

    // Support receiving ETH in case swaps route through native wrap/unwrap
    receive() external payable {}
}
