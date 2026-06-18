#!/usr/bin/env python3
"""
Crebto Bot v0.7 — Kaggle Runner
Paste this entire file into a Kaggle notebook cell and run.
"""

# ── CELL 1: Install Rust toolchain ──
import subprocess, os, sys

print("Installing Rust...")
subprocess.run([
    "sh", "-c",
    "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
], check=True)
os.environ['PATH'] += ':/root/.cargo/bin'
result = subprocess.run(["rustc", "--version"], capture_output=True, text=True)
print("Rust:", result.stdout.strip())
result = subprocess.run(["cargo", "--version"], capture_output=True, text=True)
print("Cargo:", result.stdout.strip())

# ── CELL 2: Clone repo ──
print("Cloning repo...")
os.chdir('/kaggle/working')
subprocess.run(["rm", "-rf", "crebto"], check=False)

try:
    gh_token = client.get_secret('GITHUB_TOKEN')
    clone_url = f'https://{gh_token}@github.com/yusef47/crebto.git'
    print('Cloning with token...')
except Exception:
    clone_url = 'https://github.com/yusef47/crebto.git'
    print('Cloning without token...')

subprocess.run(["git", "clone", clone_url], check=True)
os.chdir('/kaggle/working/crebto/bot')

# ── CELL 3: Read Kaggle Secrets and write .env ──
print("Reading Kaggle Secrets...")
try:
    from kaggle_secrets import UserSecretsClient
    client = UserSecretsClient()
except ImportError:
    print("ERROR: kaggle_secrets not available. Are you running on Kaggle?")
    sys.exit(1)

def get_secret(name, default=''):
    try:
        return client.get_secret(name)
    except Exception:
        return default

env_lines = [
    f"WS_RPC_URL={get_secret('WS_RPC_URL', 'wss://evm-ws.sei-apis.com')}",
    f"HTTP_RPC_URL={get_secret('HTTP_RPC_URL', 'https://evm-rpc.sei-apis.com')}",
    f"CHAIN_ID={get_secret('CHAIN_ID', '1329')}",
    f"DRY_RUN={get_secret('DRY_RUN', 'true')}",
    f"PRIVATE_KEY={get_secret('PRIVATE_KEY')}",
    f"EXECUTOR_ADDRESS={get_secret('EXECUTOR_ADDRESS')}",
    f"CONTRACT_ADDRESS={get_secret('CONTRACT_ADDRESS')}",
    f"MAX_GAS_PRICE_GWEI={get_secret('MAX_GAS_PRICE_GWEI', '100')}",
    f"EXECUTION_GAS_LIMIT={get_secret('EXECUTION_GAS_LIMIT', '600000')}",
    f"MIN_ETH_BALANCE={get_secret('MIN_ETH_BALANCE', '1.0')}",
    f"MIN_LIQUIDITY_USD={get_secret('MIN_LIQUIDITY_USD', '5000')}",
    f"MAX_LIQUIDITY_USD={get_secret('MAX_LIQUIDITY_USD', '30000')}",
    f"MIN_PROFIT_USD={get_secret('MIN_PROFIT_USD', '0.20')}",
    f"MAX_TAX_BPS={get_secret('MAX_TAX_BPS', '500')}",
    f"MAX_TRADES_PER_HOUR={get_secret('MAX_TRADES_PER_HOUR', '15')}",
    f"MAX_DAILY_LOSS_USD={get_secret('MAX_DAILY_LOSS_USD', '10.0')}",
    f"MAX_LOSS_PER_HOUR_USD={get_secret('MAX_LOSS_PER_HOUR_USD', '5.0')}",
    f"MAX_CONSECUTIVE_FAILURES={get_secret('MAX_CONSECUTIVE_FAILURES', '50')}",
    f"DRAGONSWAP_FACTORY={get_secret('DRAGONSWAP_FACTORY', '0x71f6b49ae1558357bbb5a6074f1143c46cbca03d')}",
    f"SAPPHIRE_FACTORY={get_secret('SAPPHIRE_FACTORY')}",
    f"SLIPPAGE_BPS={get_secret('SLIPPAGE_BPS', '15')}",
    f"PROBE_SIZES_USD={get_secret('PROBE_SIZES_USD', '50,100')}",
]

with open('/kaggle/working/crebto/bot/.env', 'w') as f:
    f.write('\n'.join(env_lines))

print(".env written. Key values:")
for line in env_lines:
    if any(k in line for k in ['DRY_RUN', 'DRAGONSWAP', 'WS_RPC', 'CHAIN_ID']):
        print("  ", line)

# ── CELL 4: Build ──
print("Building bot (this takes 10-15 minutes)...")
os.chdir('/kaggle/working/crebto/bot')
result = subprocess.run(
    ["cargo", "build", "--release"],
    capture_output=True, text=True
)
print(result.stdout[-2000:] if len(result.stdout) > 2000 else result.stdout)
if result.returncode != 0:
    print("BUILD FAILED:")
    print(result.stderr[-2000:])
    sys.exit(1)
print("Build successful!")

# ── CELL 5: Run bot ──
print("Starting bot...")
result = subprocess.run(
    ["cargo", "run", "--release"],
    capture_output=False,  # stream live output
)
