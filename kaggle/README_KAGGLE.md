# Running Crebto Bot v0.7 on Kaggle (Dry-Run)

This folder contains everything you need to run the Quiet Wolf Sei EVM bot on a free Kaggle notebook.

## Prerequisites

1. A Kaggle account (free tier works)
2. (Optional) Kaggle API credentials if you want to push via GitHub Actions

## Method 1: Manual Notebook (Recommended for first test)

### Step 1: Create a new Kaggle Notebook
- Go to [kaggle.com/code](https://www.kaggle.com/code)
- Click **"New Notebook"**
- Under **"Notebook"** → **"File"** → **"Upload Notebook"**, upload `kaggle_notebook.ipynb` (see below)

### Step 2: Fill in your `.env` values
In **Cell 3** of the notebook, replace the placeholder values:

```python
PRIVATE_KEY = "0xYOUR_PRIVATE_KEY"  # Only needed for LIVE mode
EXECUTOR_ADDRESS = "0xYOUR_WALLET_ADDRESS"
CONTRACT_ADDRESS = "0xYOUR_CONTRACT_ADDRESS"
```

> ⚠️ **For dry-run**, you can leave these empty. The bot will simulate trades without spending gas.

### Step 3: Run all cells
1. Turn off the **GPU/TPU** accelerator (not needed, saves quota)
2. Click **"Run All"**
3. The notebook will:
   - Install Rust (~3 min)
   - Clone your repo from GitHub (~1 min)
   - Build the bot (~10-15 min first time)
   - Start scanning DragonSwap pools on Sei EVM

### Step 4: Keep it alive
Kaggle kernels die after ~9 hours of idle time. To restart automatically:
- Use the GitHub Action `.github/workflows/restart-bot.yml` (requires Kaggle API key)
- Or manually click **"Run All"** every morning

## Method 2: Push via Kaggle CLI (for automation)

If you want GitHub Actions to automatically push the notebook to Kaggle every 12 hours:

1. Go to your Kaggle profile → **Account** → **API** → **Create New API Token**
2. Download `kaggle.json` and add these to your GitHub repo secrets:
   - `KAGGLE_USERNAME`
   - `KAGGLE_KEY`
3. The `restart-bot.yml` GitHub Action will push the notebook automatically

## What to expect

In **DRY_RUN=true** mode, the bot will:
- Connect to `wss://evm-ws.sei-apis.com`
- Discover DragonSwap pools with $5k-$30k liquidity
- Run safety checks (honeypot, tax, ownership, liquidity lock, whale filter)
- Detect swap events and simulate arbitrage opportunities
- Print profits to the notebook output (no real transactions)

Look for lines like:
```
[🎯 DRY RUN OPPORTUNITY DETECTED]
Projected NET PROFIT to Wallet: $X.XX
```

## Files in this folder

| File | Purpose |
|------|---------|
| `kaggle_notebook.ipynb` | The actual notebook to upload to Kaggle |
| `kernel-metadata.json` | Kaggle CLI metadata for automated pushes |
| `README_KAGGLE.md` | This file |
