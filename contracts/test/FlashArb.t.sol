// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {FlashArb} from "../src/FlashArb.sol";

contract FlashArbTest is Test {
    FlashArb public flashArb;

    // Base Mainnet Addresses
    address constant AAVE_POOL = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;
    address constant USDC = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    address constant WETH = 0x4200000000000000000000000000000000000006;

    function setUp() public {
        // Fork Base mainnet
        vm.createSelectFork("https://mainnet.base.org");
        flashArb = new FlashArb(AAVE_POOL);
    }

    function testDeployment() public view {
        assertEq(flashArb.owner(), address(this));
        assertEq(flashArb.pool(), AAVE_POOL);
    }

    function testRescueTokens() public {
        // Mock sending some WETH to FlashArb contract
        deal(WETH, address(flashArb), 1 ether);
        assertEq(IERC20(WETH).balanceOf(address(flashArb)), 1 ether);

        // Owner rescues tokens
        flashArb.rescueTokens(WETH);
        assertEq(IERC20(WETH).balanceOf(address(flashArb)), 0);
        assertEq(IERC20(WETH).balanceOf(address(this)), 1 ether);
    }

    function testRescueETH() public {
        // Mock sending some ETH to FlashArb contract
        deal(address(flashArb), 1 ether);
        assertEq(address(flashArb).balance, 1 ether);

        // Owner rescues ETH
        uint256 balanceBefore = address(this).balance;
        flashArb.rescueETH();
        assertEq(address(flashArb).balance, 0);
        assertEq(address(this).balance, balanceBefore + 1 ether);
    }
}

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
}
