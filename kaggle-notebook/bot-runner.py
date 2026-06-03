import os
import subprocess
import urllib.request

def run():
    print("Starting Crebto Bot Runner...")
    
    # 1. Get latest binary release URL from GitHub
    # In production, we'll download from your repo releases.
    # For now, we get it from environment or hardcoded release URL.
    binary_url = os.environ.get("BINARY_URL")
    if not binary_url:
        print("BINARY_URL environment variable is missing.")
        return

    print(f"Downloading bot binary from: {binary_url}")
    urllib.request.urlretrieve(binary_url, "/tmp/crebto-bot")
    os.chmod("/tmp/crebto-bot", 0o755)

    # 2. Setup execution environment
    env = os.environ.copy()
    
    # Run the bot and let it run for ~11.5 hours (41400 seconds)
    print("Launching bot...")
    try:
        subprocess.run(["/tmp/crebto-bot"], env=env, timeout=41400)
    except subprocess.TimeoutExpired:
        print("Bot session timeout reached (11.5 hours). Exiting for restart.")
    except Exception as e:
        print(f"Bot execution failed: {e}")

if __name__ == "__main__":
    run()
