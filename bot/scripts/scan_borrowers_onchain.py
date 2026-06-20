#!/usr/bin/env python3
"""
On-chain Aave V3 Base borrower scanner.
Scans Borrow events from the Aave V3 Pool on Base mainnet,
extracts unique user addresses, filters by debt range,
and outputs a watchlist.json for the Crebto bot.

Zero API keys needed — uses free public RPC endpoints only.
"""

import argparse
import json
import sys
import urllib.request
import urllib.error
import time

# Aave V3 Pool on Base mainnet
AAVE_POOL = "0xA238Dd80C22bdDf7D0EefB651440Ff9bA1D94454"

# Borrow event signature: Borrow(address,address,address,uint256,uint8,uint256,uint16)
BORROW_EVENT_SIG = "0x9b1bfa7fa9ee420a16e124f794c35ac9f90472acc99140eb2f6447c714cad8eb"

# getUserAccountData(address user) function signature
GET_ACCOUNT_DATA_SIG = "0xbf92857c"

# getUserReservesList(address user) function signature
GET_RESERVES_LIST_SIG = "0xd1946dbc"

# Free RPC endpoints to try
RPC_URLS = [
    "https://base.drpc.org",
    "https://base-rpc.publicnode.com",
    "https://base.llamarpc.com",
    "https://1rpc.io/base",
    "https://base.meowrpc.com",
    "https://base-mainnet.g.alchemy.com/v2/demo",
]


def rpc_call(url, method, params, retries=3):
    """Make a JSON-RPC call."""
    payload = json.dumps({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    }).encode("utf-8")

    for attempt in range(retries):
        req = urllib.request.Request(
            url,
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=20) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                if "error" in data:
                    if attempt < retries - 1:
                        time.sleep(1)
                        continue
                    return None
                return data.get("result")
        except Exception as e:
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
            print(f"  ✅ Connected to {url}")
            return url
        else:
            print(f"  ⚠️ Failed: {url}")
    return None


def fetch_borrow_events(rpc_url, from_block, to_block):
    """Fetch Borrow events from the Aave Pool in a block range."""
    params = [{
        "address": AAVE_POOL,
        "fromBlock": hex(from_block),
        "toBlock": hex(to_block),
        "topics": [BORROW_EVENT_SIG]
    }]
    logs = rpc_call(rpc_url, "eth_getLogs", params)
    if logs is None:
        return []
    return logs


def encode_get_account_data(user_address):
    """Encode getUserAccountData(address) call."""
    # Pad address to 32 bytes
    padded = "0" * 24 + user_address[2:].lower()
    return GET_ACCOUNT_DATA_SIG + padded


def encode_get_reserves_list(user_address):
    """Encode getUserReservesList(address) call."""
    padded = "0" * 24 + user_address[2:].lower()
    return GET_RESERVES_LIST_SIG + padded


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

    # Remove 0x prefix
    hex_data = result[2:] if result.startswith("0x") else result

    # getUserAccountData returns:
    # totalCollateralBase, totalDebtBase, availableBorrowsBase,
    # currentLiquidationThreshold, ltv, healthFactor
    # Each is uint256 (64 hex chars)
    try:
        total_collateral = int(hex_data[0:64], 16)
        total_debt = int(hex_data[64:128], 16)
        available_borrows = int(hex_data[128:192], 16)
        liquidation_threshold = int(hex_data[192:256], 16)
        ltv = int(hex_data[256:320], 16)
        health_factor = int(hex_data[320:384], 16)
        return {
            "totalCollateralBase": total_collateral,
            "totalDebtBase": total_debt,
            "availableBorrowsBase": available_borrows,
            "currentLiquidationThreshold": liquidation_threshold,
            "ltv": ltv,
            "healthFactor": health_factor,
        }
    except (ValueError, IndexError):
        return None


def scan_borrowers(min_debt_usd, max_debt_usd, limit, rpc_url):
    """
    Scan Borrow events on Base Aave V3, discover active borrowers,
    filter by debt range, and return addresses.
    """
    # Get current block
    current_block_hex = rpc_call(rpc_url, "eth_blockNumber", [])
    if not current_block_hex:
        print("❌ Failed to get current block number")
        return []
    current_block = int(current_block_hex, 16)
    print(f"Current block: {current_block}")

    # Scan last ~2000 blocks (~5-6 hours on Base)
    scan_blocks = 2000
    from_block = max(current_block - scan_blocks, 0)

    print(f"Scanning Borrow events from block {from_block} to {current_block}...")

    # Fetch logs in batches to avoid RPC limits
    batch_size = 500
    all_users = set()

    for batch_start in range(from_block, current_block, batch_size):
        batch_end = min(batch_start + batch_size - 1, current_block)
        logs = fetch_borrow_events(rpc_url, batch_start, batch_end)

        for log in logs:
            # Borrow event: indexed user is topic[1]
            if len(log.get("topics", [])) >= 2:
                topic = log["topics"][1]
                # Extract address from topic (last 20 bytes of 32-byte word)
                addr = "0x" + topic[-40:]
                all_users.add(addr.lower())

        # Throttle
        time.sleep(0.3)

    print(f"Found {len(all_users)} unique borrowers in {scan_blocks} blocks")

    if not all_users:
        print("⚠️ No Borrow events found. Try scanning more blocks.")
        return []

    # Filter by debt range
    print(f"Filtering by debt range: ${min_debt_usd:,.0f} – ${max_debt_usd:,.0f}")
    users_with_debt = {}

    for i, user in enumerate(list(all_users)):
        if i % 50 == 0:
            print(f"  Checking user {i+1}/{len(all_users)}...")
        data = get_user_account_data(rpc_url, user)
        if data:
            # totalDebtBase is in USD with 8 decimals (like Aave's internal oracle)
            debt_usd = data["totalDebtBase"] / 1e8
            if min_debt_usd <= debt_usd <= max_debt_usd:
                users_with_debt[user] = {
                    "debt_usd": debt_usd,
                    "health_factor": data["healthFactor"] / 1e18,
                }
        time.sleep(0.05)  # Rate limit

    print(f"Found {len(users_with_debt)} borrowers in debt range")

    # Sort by debt (highest first) and limit
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
    parser.add_argument("--output", type=str, default="/kaggle/working/watchlist.json",
                        help="Output JSON file")
    args = parser.parse_args()

    print("═══ On-Chain Aave V3 Base Borrower Scanner ═══")
    print()

    # Find working RPC
    print("Finding working RPC endpoint...")
    rpc_url = find_working_rpc()
    if not rpc_url:
        print("❌ No working RPC endpoints found. Cannot continue.")
        sys.exit(1)

    print()
    addresses = scan_borrowers(args.min, args.max, args.limit, rpc_url)

    if not addresses:
        print()
        print("❌ No borrowers found. Possible causes:")
        print("  1. No Borrow events in the scanned block range")
        print("  2. RPC rate limiting")
        print("  3. No borrowers match the debt range")
        print()
        print("Fallback: creating minimal watchlist so the bot can start.")
        addresses = ["0x0000000000000000000000000000000000000000"]

    # Save
    with open(args.output, "w") as f:
        json.dump(addresses, f, indent=2)

    print()
    print(f"✅ Watchlist saved to {args.output} ({len(addresses)} addresses)")


if __name__ == "__main__":
    main()
