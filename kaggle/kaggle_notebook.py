# ═══════════════════════════════════════════════════════
# Kaggle Notebook: Crebto Bot v0.7 Sei EVM Dry-Run
# Paste these cells into a Kaggle notebook and run sequentially
# ═══════════════════════════════════════════════════════

# ── CELL 1: Clone repo (or upload source via Kaggle Dataset) ──
# If using Git:
# !git clone https://github.com/YOUR_USERNAME/crebto.git
# %cd crebto/bot

# If uploading source as Kaggle Dataset, skip this cell.

# ── CELL 2: Install Rust toolchain ──
!curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
import os
os.environ['PATH'] += ':/root/.cargo/bin'
!rustc --version && cargo --version

# ── CELL 3: Verify RPC connectivity ──
import urllib.request, json
req = json.dumps({"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}).encode()
resp = urllib.request.urlopen(urllib.request.Request(
    'https://evm-rpc.sei-apis.com/',
    data=req, headers={'Content-Type':'application/json'}, timeout=15
))
print("Block:", json.loads(resp.read())['result'])

# ── CELL 4: Write .env file ──
env_content = """
WS_RPC_URL=wss://evm-ws.sei-apis.com
HTTP_RPC_URL=https://evm-rpc.sei-apis.com
CHAIN_ID=1329
DRY_RUN=true
MAX_GAS_PRICE_GWEI=100
EXECUTION_GAS_LIMIT=600000
MIN_ETH_BALANCE=1.0
MIN_LIQUIDITY_USD=5000
MAX_LIQUIDITY_USD=30000
MIN_PROFIT_USD=0.20
MAX_TAX_BPS=500
MAX_TRADES_PER_HOUR=15
MAX_DAILY_LOSS_USD=10.0
MAX_LOSS_PER_HOUR_USD=5.0
MAX_CONSECUTIVE_FAILURES=50
DRAGONSWAP_FACTORY=0x71f6b49ae1558357bbb5a6074f1143c46cbca03d
SAPPHIRE_FACTORY=
SLIPPAGE_BPS=15
PROBE_SIZES_USD=50,100
"""
with open('/kaggle/working/crebto/bot/.env', 'w') as f:
    f.write(env_content.strip())
print(".env written")

# ── CELL 5: Build the bot ──
%cd /kaggle/working/crebto/bot
!cargo build --release 2>&1 | tail -20

# ── CELL 6: Run dry-run bot ──
# This will run until the Kaggle kernel dies (~9-12 hours)
!cargo run --release 2>&1 | tee /kaggle/working/bot_log.txt
