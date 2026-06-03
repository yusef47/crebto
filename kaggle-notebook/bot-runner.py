import os
import subprocess
import urllib.request
import json

def get_latest_binary_info(github_token):
    try:
        api_url = "https://api.github.com/repos/yusef47/crebto/releases/latest"
        headers = {
            'User-Agent': 'Crebto-Bot-Runner'
        }
        if github_token:
            headers['Authorization'] = f'token {github_token}'
            
        req = urllib.request.Request(api_url, headers=headers)
        with urllib.request.urlopen(req) as response:
            release_info = json.loads(response.read().decode())
            for asset in release_info.get("assets", []):
                if asset.get("name") == "crebto-bot":
                    return asset.get("id"), asset.get("browser_download_url")
    except Exception as e:
        print(f"Error fetching latest release metadata from GitHub: {e}")
    return None, None

def download_private_binary(asset_id, github_token, output_path):
    try:
        url = f"https://api.github.com/repos/yusef47/crebto/releases/assets/{asset_id}"
        req = urllib.request.Request(
            url,
            headers={
                'User-Agent': 'Crebto-Bot-Runner',
                'Authorization': f'token {github_token}',
                'Accept': 'application/octet-stream'
            }
        )
        with urllib.request.urlopen(req) as response:
            with open(output_path, 'wb') as f:
                f.write(response.read())
        return True
    except Exception as e:
        print(f"Error downloading asset {asset_id} from GitHub: {e}")
        return False

def run():
    print("Starting Crebto Bot Runner...")
    
    # 1. Setup execution environment and Kaggle Secrets
    env = os.environ.copy()
    github_token = None
    
    try:
        from kaggle_secrets import UserSecretsClient
        user_secrets = UserSecretsClient()
        
        # Load GitHub Token
        github_token = user_secrets.get_secret("GITHUB_TOKEN")
        if github_token:
            print("🔑 GitHub Token loaded successfully.")
        else:
            print("⚠️ Warning: GITHUB_TOKEN not found in Kaggle Secrets.")
            
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
        github_token = os.environ.get("GITHUB_TOKEN")

    # 2. Fetch latest binary release URL / Asset ID from GitHub
    asset_id, browser_url = get_latest_binary_info(github_token)
    
    if not asset_id:
        print("❌ Error: Could not retrieve release asset info from GitHub.")
        return

    # 3. Download the binary
    print(f"📥 Downloading bot binary asset ID: {asset_id}...")
    success = False
    if github_token:
        # Use private download method
        success = download_private_binary(asset_id, github_token, "/tmp/crebto-bot")
    else:
        # Try public fallback download
        try:
            urllib.request.urlretrieve(browser_url, "/tmp/crebto-bot")
            success = True
        except Exception as e:
            print(f"Public download fallback failed: {e}")

    if not success:
        print("❌ Error: Failed to download the binary.")
        return

    os.chmod("/tmp/crebto-bot", 0o755)
    print("✅ Binary downloaded and permissions set.")

    # 4. Launch the bot
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
