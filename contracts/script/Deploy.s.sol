// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script} from "forge-std/Script.sol";
import {FlashArb} from "../src/FlashArb.sol";

contract DeployFlashArb is Script {
    // Aave V3 Pool address on Base Mainnet
    address constant AAVE_POOL = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        
        vm.startBroadcast(deployerPrivateKey);

        FlashArb flashArb = new FlashArb(AAVE_POOL);

        vm.stopBroadcast();
    }
}
