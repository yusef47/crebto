import os
import subprocess
import urllib.request
import json

def get_latest_binary_url():
    try:
        api_url = "https://api.github.com/repos/yusef47/crebto/releases/latest"
        req = urllib.request.Request(
            api_url, 
            headers={'User-Agent': 'Crebto-Bot-Runner'}
        )
        with urllib.request.urlopen(req) as response:
            release_info = json.loads(response.read().decode())
            for asset in release_info.get("assets", []):
                if asset.get("name") == "crebto-bot":
                    return asset.get("browser_download_url")
    except Exception as e:
        print(f"Error fetching latest release from GitHub: {e}")
    return None

def run():
    print("Starting Crebto Bot Runner...")
    
    # 1. Fetch latest binary release URL from GitHub
    binary_url = get_latest_binary_url()
    if not binary_url:
        # Fallback to env variable
        binary_url = os.environ.get("BINARY_URL")
        
    if not binary_url:
        print("❌ Error: Could not find latest release binary url.")
        return

    print(f"📥 Downloading bot binary from: {binary_url}")
    urllib.request.urlretrieve(binary_url, "/tmp/crebto-bot")
    os.chmod("/tmp/crebto-bot", 0o755)
    print("✅ Binary downloaded and permissions set.")

    # 2. Setup execution environment and Kaggle Secrets
    env = os.environ.copy()
    try:
        from kaggle_secrets import UserSecretsClient
        user_secrets = UserSecretsClient()
        
        # Load Alchemy WSS URL
        alchemy_wss = user_secrets.get_secret("ALCHEMY_WSS")
        if alchemy_wss:
            env["ALCHEMY_WSS"] = alchemy_wss
            print("🔑 Alchemy WSS loaded from Kaggle Secrets.")
        else:
            print("⚠️ Warning: ALCHEMY_WSS not found in Kaggle Secrets.")
            
        # Load Dry Run mode
        dry_run = user_secrets.get_secret("DRY_RUN")
        env["DRY_RUN"] = dry_run if dry_run else "true"
        print(f"⚙️ DRY_RUN mode set to: {env['DRY_RUN']}")
        
    except Exception as e:
        print(f"ℹ️ Kaggle secrets module not active. Using existing system env variables. Details: {e}")

    # 3. Launch the bot
    print("🚀 Launching bot...")
    try:
        # Run for ~11.5 hours (41400 seconds)
        subprocess.run(["/tmp/crebto-bot"], env=env, timeout=41400)
    except subprocess.TimeoutExpired:
        print("⏳ Bot session timeout reached (11.5 hours). Exiting for restart.")
    except Exception as e:
        print(f"❌ Bot execution failed: {e}")

if __name__ == "__main__":
    run()
