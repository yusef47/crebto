#!/usr/bin/env python3
"""
Generate watchlist.json for Crebto bot by querying the Aave V3 Base subgraph.
Filters for active borrowers with $1,000–$5,000 total debt USD.

NOTE: The Graph hosted service (api.thegraph.com) was deprecated in 2023.
For production use, get a free API key from https://thegraph.com/studio/
and use the decentralized network endpoint:
  https://gateway.thegraph.com/api/<YOUR_API_KEY>/subgraphs/id/GQFbb95cE6d8mV989mL5figjaGaKCQB3xqYrr1bRyXqF

Usage:
    python generate_watchlist.py --min 1000 --max 5000 --limit 300 --output /kaggle/working/watchlist.json
"""

import argparse
import json
import os
import sys
import urllib.request
import urllib.error

# Subgraph endpoints — tries hosted service first, then decentralized network (if API key set)
_SUBGRAPH_ID = "GQFbb95cE6d8mV989mL5figjaGaKCQB3xqYrr1bRyXqF"
_SUBGRAPH_API_KEY = os.environ.get("GRAPH_API_KEY", "")

SUBGRAPH_ENDPOINTS = ["https://api.thegraph.com/subgraphs/name/aave/protocol-v3-base"]
if _SUBGRAPH_API_KEY:
    SUBGRAPH_ENDPOINTS.insert(
        0,
        f"https://gateway.thegraph.com/api/{_SUBGRAPH_API_KEY}/subgraphs/id/{_SUBGRAPH_ID}"
    )

USER_RESERVES_QUERY = """
query GetActiveBorrowers($skip: Int!, $first: Int!) {
  userReserves(
    where: {
      currentTotalDebt_gt: "0"
    }
    first: $first
    skip: $skip
    orderBy: currentTotalDebt
    orderDirection: desc
  ) {
    user {
      id
    }
    reserve {
      symbol
      decimals
      price {
        priceInUSD
      }
    }
    currentVariableDebt
    currentStableDebt
    currentTotalDebt
    usageAsCollateralEnabled
    currentATokenBalance
  }
}
"""


def fetch_user_reserves(skip: int, first: int = 1000):
    """Fetch a batch of userReserves from the subgraph (tries multiple endpoints)."""
    payload = json.dumps({
        "query": USER_RESERVES_QUERY,
        "variables": {"skip": skip, "first": first}
    }).encode("utf-8")

    for url in SUBGRAPH_ENDPOINTS:
        req = urllib.request.Request(
            url,
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                if "errors" in data:
                    print(f"Subgraph error on {url}: {data['errors']}", file=sys.stderr)
                    continue
                return data.get("data", {}).get("userReserves", [])
        except urllib.error.HTTPError as e:
            body = e.read().decode("utf-8")
            print(f"HTTP {e.code} from {url}: {body}", file=sys.stderr)
        except Exception as e:
            print(f"Request failed for {url}: {e}", file=sys.stderr)

    return []


def generate_watchlist(min_debt_usd: float, max_debt_usd: float, limit: int):
    """
    Aggregate user reserves by borrower, calculate total debt in USD,
    and return addresses within the target range.
    """
    print(f"Fetching borrowers from Aave V3 Base subgraph...")
    print(f"Target: ${min_debt_usd:,.0f}–${max_debt_usd:,.0f} debt | Limit: {limit}")

    all_reserves = []
    skip = 0
    batch_size = 1000

    # Fetch up to ~10,000 records to get good coverage
    while skip < 10000:
        print(f"  Fetching skip={skip}...")
        batch = fetch_user_reserves(skip, batch_size)
        if not batch:
            break
        all_reserves.extend(batch)
        skip += batch_size
        if len(batch) < batch_size:
            break

    print(f"Fetched {len(all_reserves)} userReserve records. Aggregating...")

    # Aggregate by user
    users = {}
    for ur in all_reserves:
        user_id = ur["user"]["id"]
        reserve = ur["reserve"]
        decimals = int(reserve["decimals"])
        price_usd = float(reserve["price"]["priceInUSD"])

        variable_debt_raw = int(ur.get("currentVariableDebt", "0"))
        stable_debt_raw = int(ur.get("currentStableDebt", "0"))
        a_token_raw = int(ur.get("currentATokenBalance", "0"))

        variable_debt = variable_debt_raw / (10 ** decimals)
        stable_debt = stable_debt_raw / (10 ** decimals)
        a_token_bal = a_token_raw / (10 ** decimals)

        debt_usd = (variable_debt + stable_debt) * price_usd
        collateral_usd = a_token_bal * price_usd

        if user_id not in users:
            users[user_id] = {
                "total_debt_usd": 0.0,
                "total_collateral_usd": 0.0,
                "reserves": 0,
            }

        users[user_id]["total_debt_usd"] += debt_usd
        users[user_id]["total_collateral_usd"] += collateral_usd
        users[user_id]["reserves"] += 1

    # Filter by debt range
    filtered = [
        addr for addr, data in users.items()
        if min_debt_usd <= data["total_debt_usd"] <= max_debt_usd
    ]

    # Sort by total debt (descending) and limit
    filtered.sort(key=lambda a: users[a]["total_debt_usd"], reverse=True)
    selected = filtered[:limit]

    print(f"\nFound {len(filtered)} borrowers in range. Selected top {len(selected)}.")
    if selected:
        print(f"Debt range of selected: ${users[selected[-1]]['total_debt_usd']:,.2f} – ${users[selected[0]]['total_debt_usd']:,.2f}")
    else:
        print("No borrowers matched the criteria.")

    return selected


def main():
    parser = argparse.ArgumentParser(description="Generate Crebto watchlist.json")
    parser.add_argument("--min", type=float, default=1000, help="Minimum debt USD")
    parser.add_argument("--max", type=float, default=5000, help="Maximum debt USD")
    parser.add_argument("--limit", type=int, default=300, help="Max borrowers in watchlist")
    parser.add_argument("--output", type=str, default="/kaggle/working/watchlist.json",
                        help="Output JSON file path")
    args = parser.parse_args()

    addresses = generate_watchlist(args.min, args.max, args.limit)

    if not addresses:
        print(
            "\n❌ No borrowers found. Possible causes:\n"
            "  1. All subgraph endpoints failed. Set GRAPH_API_KEY env var\n"
            "     for The Graph decentralized network (get free key at https://thegraph.com/studio/)\n"
            "  2. No borrowers match the debt range. Try widening --min and --max.\n",
            file=sys.stderr
        )
        sys.exit(1)

    # Save as simple array of addresses (the bot expects this format)
    with open(args.output, "w") as f:
        json.dump(addresses, f, indent=2)

    print(f"\n✅ Watchlist saved to {args.output} ({len(addresses)} addresses)")


if __name__ == "__main__":
    main()
