import json
import os
import subprocess
import urllib.request


REPO = "yusef47/crebto"
BINARY_NAME = "crebto-bot"
BINARY_PATH = "/tmp/crebto-bot"

SECRET_NAMES = [
    "GITHUB_TOKEN",
    "ALCHEMY_WSS",
    "ALCHEMY_HTTP",
    "DRY_RUN",
    "REQUIRE_SIMULATION",
    "ENABLE_LIVE_SEND",
    "PRIVATE_KEY",
    "EXECUTOR_ADDRESS",
    "CONTRACT_ADDRESS",
    "UNISWAP_V3_ROUTER",
    "AERODROME_ROUTER",
    "AERODROME_SLIPSTREAM_ROUTER",
    "AERODROME_FACTORY",
    "MAX_GAS_PRICE_GWEI",
    "MIN_ETH_BALANCE",
    "MAX_LOSS_PER_HOUR_USD",
    "MAX_CONSECUTIVE_FAILURES",
    "MIN_PROFIT_USD",
    "EXECUTION_GAS_LIMIT",
    "SLIPPAGE_BPS",
    "PROBE_SIZES_USD",
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_CHAT_ID",
    # v0.6 safety & live trading secrets
    "MIN_LIQUIDITY_USD",
    "MAX_TAX_BPS",
    "MAX_DAILY_LOSS_USD",
    "MAX_TRADES_PER_HOUR",
]

DEFAULTS = {
    "DRY_RUN": "true",
    "REQUIRE_SIMULATION": "true",
    "ENABLE_LIVE_SEND": "false",
    "MAX_GAS_PRICE_GWEI": "100",
    "MIN_ETH_BALANCE": "0.005",
    "MAX_LOSS_PER_HOUR_USD": "5.0",
    "MAX_CONSECUTIVE_FAILURES": "50",
    "MIN_PROFIT_USD": "0.20",
    "EXECUTION_GAS_LIMIT": "600000",
    "SLIPPAGE_BPS": "15",
    "PROBE_SIZES_USD": "50,100",
    # v0.6: Safety & live trading defaults
    "MIN_LIQUIDITY_USD": "5000",
    "MAX_TAX_BPS": "500",
    "MAX_DAILY_LOSS_USD": "10.0",
    "MAX_TRADES_PER_HOUR": "5",
}

MASKED_NAMES = {
    "GITHUB_TOKEN",
    "PRIVATE_KEY",
    "TELEGRAM_BOT_TOKEN",
}


def mask_value(name, value):
    if not value:
        return "<missing>"
    if name in MASKED_NAMES:
        return "<loaded>"
    return value


def get_latest_binary_info(github_token):
    try:
        api_url = f"https://api.github.com/repos/{REPO}/releases/latest"
        headers = {"User-Agent": "Crebto-Bot-Runner"}
        if github_token:
            headers["Authorization"] = f"token {github_token}"

        req = urllib.request.Request(api_url, headers=headers)
        with urllib.request.urlopen(req, timeout=30) as response:
            release_info = json.loads(response.read().decode())
            for asset in release_info.get("assets", []):
                if asset.get("name") == BINARY_NAME:
                    return asset.get("id"), asset.get("browser_download_url")
    except Exception as exc:
        print(f"Error fetching latest release metadata from GitHub: {exc}")
    return None, None


def download_private_binary(asset_id, github_token, output_path):
    try:
        url = f"https://api.github.com/repos/{REPO}/releases/assets/{asset_id}"
        req = urllib.request.Request(
            url,
            headers={
                "User-Agent": "Crebto-Bot-Runner",
                "Authorization": f"token {github_token}",
                "Accept": "application/octet-stream",
            },
        )
        with urllib.request.urlopen(req, timeout=60) as response:
            with open(output_path, "wb") as file:
                file.write(response.read())
        return True
    except Exception as exc:
        print(f"Error downloading asset {asset_id} from GitHub: {exc}")
        return False


def load_kaggle_secrets(env):
    github_token = os.environ.get("GITHUB_TOKEN")

    try:
        from kaggle_secrets import UserSecretsClient

        user_secrets = UserSecretsClient()
        for name in SECRET_NAMES:
            try:
                value = user_secrets.get_secret(name)
            except Exception:
                value = None

            if value:
                env[name] = value
                if name == "GITHUB_TOKEN":
                    github_token = value
                print(f"{name}: {mask_value(name, value)}")
    except Exception as exc:
        print(f"Kaggle secrets module not active. Using environment variables. Details: {exc}")

    for name, value in DEFAULTS.items():
        if not env.get(name):
            env[name] = value
            print(f"{name}: defaulted to {value}")

    if not env.get("ALCHEMY_HTTP") and env.get("ALCHEMY_WSS"):
        env["ALCHEMY_HTTP"] = env["ALCHEMY_WSS"].replace("wss://", "https://").replace("/ws/", "/")
        print("ALCHEMY_HTTP: derived from ALCHEMY_WSS")

    return github_token


def validate_runtime(env):
    required = ["ALCHEMY_WSS", "ALCHEMY_HTTP"]
    missing = [name for name in required if not env.get(name)]
    if missing:
        print(f"Error: missing required secrets/env values: {', '.join(missing)}")
        return False

    print("Runtime safety:")
    print(f"  DRY_RUN={env.get('DRY_RUN')}")
    print(f"  REQUIRE_SIMULATION={env.get('REQUIRE_SIMULATION')}")
    print(f"  ENABLE_LIVE_SEND={env.get('ENABLE_LIVE_SEND')}")

    if env.get("ENABLE_LIVE_SEND", "false").lower() == "true":
        live_required = [
            "PRIVATE_KEY",
            "EXECUTOR_ADDRESS",
            "CONTRACT_ADDRESS",
            "UNISWAP_V3_ROUTER",
            "AERODROME_ROUTER",
            "AERODROME_SLIPSTREAM_ROUTER",
            "AERODROME_FACTORY",
        ]
        live_missing = [name for name in live_required if not env.get(name)]
        if live_missing:
            print(f"Error: live send is enabled but these values are missing: {', '.join(live_missing)}")
            return False

    return True


def download_binary(github_token):
    asset_id, browser_url = get_latest_binary_info(github_token)
    if not asset_id:
        print("Error: could not retrieve release asset info from GitHub.")
        return False

    print(f"Downloading bot binary asset ID: {asset_id}")
    if github_token:
        success = download_private_binary(asset_id, github_token, BINARY_PATH)
    else:
        try:
            urllib.request.urlretrieve(browser_url, BINARY_PATH)
            success = True
        except Exception as exc:
            print(f"Public download fallback failed: {exc}")
            success = False

    if not success:
        print("Error: failed to download the binary.")
        return False

    os.chmod(BINARY_PATH, 0o755)
    print("Binary downloaded and permissions set.")
    return True


def run_bot(env):
    print("Launching bot...")
    process = None
    try:
        process = subprocess.Popen(
            [BINARY_PATH],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )

        while True:
            line = process.stdout.readline()
            if not line and process.poll() is not None:
                break
            if line:
                print(line.strip(), flush=True)

        process.wait(timeout=41400)
        print("Bot execution session finished.")
    except subprocess.TimeoutExpired:
        print("Bot session timeout reached (11.5 hours). Terminating.")
        if process:
            process.terminate()
    except Exception as exc:
        print(f"Bot execution failed: {exc}")


def run():
    print("Starting Crebto Bot Runner...")
    env = os.environ.copy()
    github_token = load_kaggle_secrets(env)

    if not validate_runtime(env):
        return

    if not download_binary(github_token):
        return

    run_bot(env)


if __name__ == "__main__":
    run()
