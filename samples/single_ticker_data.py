import os
from datetime import datetime, timedelta, timezone
import requests
import pandas as pd

# 1. Retrieve API key from environment
API_KEY = os.getenv("MASSIVE_API_KEY") or os.getenv("POLYGON_API_KEY")

if not API_KEY:
    raise ValueError("Environment variable MASSIVE_API_KEY is missing.")

# 2. Configure request parameters
TICKER = "AAPL"        # Target asset symbol
MULTIPLIER = 15        # Bar length (15)
TIMESPAN = "minute"    # Bar unit (minute)

# Set date range for the last 7 days (YYYY-MM-DD)
now = datetime.now(timezone.utc)
days_ago = now - timedelta(days=60)

from_date = days_ago.strftime("%Y-%m-%d")
to_date = now.strftime("%Y-%m-%d")

# 3. Request data from Massive REST endpoint
url = f"https://api.massive.com/v2/aggs/ticker/{TICKER}/range/{MULTIPLIER}/{TIMESPAN}/{from_date}/{to_date}"

params = {
    "adjusted": "true",
    "sort": "asc",
    "limit": 50000,
    "apiKey": API_KEY
}

response = requests.get(url, params=params)
data = response.json()

if response.status_code != 200 or "results" not in data:
    raise RuntimeError(f"API Request Failed ({response.status_code}): {data.get('error', data.get('status', 'Unknown error'))}")

# 4. Transform response payload to match target schema
rows = []
for bar in data["results"]:
    rows.append({
        "timestamp": int(bar["t"] / 1000),  # Convert milliseconds -> Unix seconds
        "open": round(bar["o"], 2),
        "high": round(bar["h"], 2),
        "low": round(bar["l"], 2),
        "close": round(bar["c"], 2),
        "volume": int(bar["v"])
    })

# 5. Export to CSV
df = pd.DataFrame(rows)
output_file = f"{TICKER.lower()}_15m_data.csv"
df.to_csv(output_file, index=False)

print(f"Exported {len(df)} bars to '{output_file}' successfully.")
