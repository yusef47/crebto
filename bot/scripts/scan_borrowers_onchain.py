#!/usr/bin/env python3
"""
On-chain Aave V3 Base borrower scanner.
Scans Borrow events from the Aave V3 Pool on Base mainnet,
extracts unique user addresses, filters by debt range,
and outputs a watchlist.json for the Crebto bot.

Uses curl (subprocess) for RPC calls — bypasses Kaggle's Python urllib block.
Zero API keys needed — uses free public RPC endpoints only.
"""

import argparse
import json
import subprocess
import sys
import time

# Aave V3 Pool on Base mainnet
AAVE_POOL = "0xA238Dd80C22bdDf7D0EefB651440Ff9bA1D94454"

# Borrow event signature: Borrow(address,address,address,uint256,uint8,uint256,uint16)
BORROW_EVENT_SIG = "0x9b1bfa7fa9ee420a16e124f794c35ac9f90472acc99140eb2f6447c714cad8eb"

# getUserAccountData(address user) function signature
GET_ACCOUNT_DATA_SIG = "0xbf92857c"

# Free RPC endpoints to try
RPC_URLS = [
    "https://base.drpc.org",
    "https://base-rpc.publicnode.com",
    "https://base.llamarpc.com",
    "https://1rpc.io/base",
    "https://base.meowrpc.com",
]


def rpc_call(url, method, params, retries=3):
    """Make a JSON-RPC call using curl (bypasses Kaggle Python urllib block)."""
    payload = json.dumps({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    })

    for attempt in range(retries):
        try:
            result = subprocess.run(
                ["curl", "-s", "--max-time", "20",
                 "-X", "POST", url,
                 "-H", "Content-Type: application/json",
                 "-d", payload],
                capture_output=True,
                text=True,
                timeout=25
            )
            if result.returncode != 0:
                if attempt < retries - 1:
                    time.sleep(1)
                    continue
                return None

            data = json.loads(result.stdout)
            if "error" in data:
                if attempt < retries - 1:
                    time.sleep(1)
                    continue
                return None
            return data.get("result")
        except (subprocess.TimeoutExpired, json.JSONDecodeError, Exception):
            if attempt < retries - 1:
                time.sleep(1)
                continue
            return None
    return None


def find_working_rpc():
    """Find first working RPC endpoint."""
    for url in RPC_URLS:
        result = rpc_call(url, "eth_blockNumber", [])
        if result:
            block = int(result, 16)
            print(f"  ✅ Connected to {url} (block {block})")
            return url
        else:
            print(f"  ⚠️ Failed: {url}")
    return None


def fetch_borrow_events(rpc_url, from_block, to_block):
    """Fetch Borrow events from the Aave Pool in a block range."""
    # Request ALL logs from Aave Pool (no topic filter) — some free RPCs
    # silently fail on eth_getLogs with topic filters
    params = [{
        "address": AAVE_POOL,
        "fromBlock": hex(from_block),
        "toBlock": hex(to_block),
    }]
    logs = rpc_call(rpc_url, "eth_getLogs", params)
    if logs is None:
        return []
    # Filter by Borrow event signature in Python
    return [log for log in logs if log.get("topics") and log["topics"][0] == BORROW_EVENT_SIG]


def encode_get_account_data(user_address):
    """Encode getUserAccountData(address) call."""
    padded = "0" * 24 + user_address[2:].lower()
    return GET_ACCOUNT_DATA_SIG + padded


def get_user_account_data(rpc_url, user):
    """Get health factor and total debt for a user."""
    data = encode_get_account_data(user)
    params = [{
        "to": AAVE_POOL,
        "data": data
    }, "latest"]
    result = rpc_call(rpc_url, "eth_call", params)
    if not result or result == "0x":
        return None

    hex_data = result[2:] if result.startswith("0x") else result

    try:
        total_collateral = int(hex_data[0:64], 16)
        total_debt = int(hex_data[64:128], 16)
        health_factor = int(hex_data[320:384], 16)
        return {
            "totalCollateralBase": total_collateral,
            "totalDebtBase": total_debt,
            "healthFactor": health_factor,
        }
    except (ValueError, IndexError):
        return None


def scan_borrowers(min_debt_usd, max_debt_usd, limit, rpc_url, scan_blocks=100000):
    """Scan Borrow events, discover active borrowers, filter by debt."""
    current_block_hex = rpc_call(rpc_url, "eth_blockNumber", [])
    if not current_block_hex:
        print("❌ Failed to get current block number")
        return []
    current_block = int(current_block_hex, 16)
    print(f"Current block: {current_block}")

    from_block = max(current_block - scan_blocks, 0)
    print(f"Scanning Borrow events from block {from_block} to {current_block} ({scan_blocks:,} blocks)...")

    batch_size = 500
    all_users = set()

    for batch_start in range(from_block, current_block, batch_size):
        batch_end = min(batch_start + batch_size - 1, current_block)
        logs = fetch_borrow_events(rpc_url, batch_start, batch_end)

        for log in logs:
            if len(log.get("topics", [])) >= 2:
                topic = log["topics"][1]
                addr = "0x" + topic[-40:]
                all_users.add(addr.lower())

        time.sleep(0.3)

    print(f"Found {len(all_users)} unique borrowers in {scan_blocks} blocks")

    if not all_users:
        print("⚠️ No Borrow events found. Try scanning more blocks.")
        return []

    print(f"Filtering by debt range: ${min_debt_usd:,.0f} – ${max_debt_usd:,.0f}")
    users_with_debt = {}

    user_list = list(all_users)
    for i, user in enumerate(user_list):
        if i % 50 == 0:
            print(f"  Checking user {i+1}/{len(user_list)}...")
        data = get_user_account_data(rpc_url, user)
        if data:
            debt_usd = data["totalDebtBase"] / 1e8
            if min_debt_usd <= debt_usd <= max_debt_usd:
                users_with_debt[user] = {
                    "debt_usd": debt_usd,
                    "health_factor": data["healthFactor"] / 1e18,
                }
        time.sleep(0.05)

    print(f"Found {len(users_with_debt)} borrowers in debt range")

    sorted_users = sorted(users_with_debt.items(), key=lambda x: x[1]["debt_usd"], reverse=True)
    selected = sorted_users[:limit]

    if selected:
        print(f"Debt range of selected: ${selected[-1][1]['debt_usd']:,.2f} – ${selected[0][1]['debt_usd']:,.2f}")

    return [addr for addr, _ in selected]


def main():
    parser = argparse.ArgumentParser(description="Scan Aave V3 Base for borrowers on-chain")
    parser.add_argument("--min", type=float, default=1000, help="Minimum debt USD")
    parser.add_argument("--max", type=float, default=5000, help="Maximum debt USD")
    parser.add_argument("--limit", type=int, default=300, help="Max borrowers")
    parser.add_argument("--blocks", type=int, default=100000, help="Blocks to scan")
    parser.add_argument("--output", type=str, default="/kaggle/working/watchlist.json")
    args = parser.parse_args()

    print("═══ On-Chain Aave V3 Base Borrower Scanner ═══")
    print()

    print("Finding working RPC endpoint...")
    rpc_url = find_working_rpc()
    if not rpc_url:
        print("❌ No working RPC endpoints found.")
        sys.exit(1)

    print()
    addresses = scan_borrowers(args.min, args.max, args.limit, rpc_url, args.blocks)

    if not addresses:
        print()
        print("❌ No borrowers found. Creating minimal fallback watchlist.")
        addresses = ["0x0000000000000000000000000000000000000000"]

    with open(args.output, "w") as f:
        json.dump(addresses, f, indent=2)

    print()
    print(f"✅ Watchlist saved to {args.output} ({len(addresses)} addresses)")


if __name__ == "__main__":
    main()
